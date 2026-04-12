//! macOS text injection — CGEvent Cmd+V + NSRunningApplication.
//!
//! Uses:
//!   - NSWorkspace to track the frontmost application
//!   - NSRunningApplication to restore focus
//!   - CoreGraphics CGEvent to simulate Cmd+V keystrokes
//!   - pbcopy/pbpaste for clipboard access (subprocess-safe, avoids ObjC exceptions)
//!
//! Requires Accessibility access (System Preferences > Privacy & Security > Accessibility).

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::Duration;

use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};

use crate::config;
use super::CapturedTarget;

// ── macOS virtual key codes ───────────────────────────────────────────────
const KVK_COMMAND: u16 = 0x37;
const KVK_ANSI_V: u16 = 0x09;

// CGEvent modifier flags
const KCGEVENT_FLAG_MASK_COMMAND: u64 = 1 << 20;

// ── CoreGraphics FFI ──────────────────────────────────────────────────────
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: *const c_void,
        virtual_key: u16,
        key_down: bool,
    ) -> *mut c_void;
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
}

// CGEventTapLocation — post to the HID event stream (delivers to focused app)
const KCG_HID_EVENT_TAP: u32 = 0;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

// ── Focus tracker globals ─────────────────────────────────────────────────
static OWN_PID: AtomicI32 = AtomicI32::new(0);
static LAST_EXT_PID: AtomicI32 = AtomicI32::new(0);

/// Spawn a background thread that polls `NSWorkspace.frontmostApplication`
/// every 200 ms and tracks the last external (non-Furo) app PID.
///
/// ObjC calls are wrapped in:
///  - NSAutoreleasePool: required on Rust-created threads (no pool by default)
///  - std::panic::catch_unwind: with the `exception` feature, ObjC NSExceptions
///    become Rust panics; catch_unwind keeps the loop alive if one occurs
pub fn start_focus_tracker() {
    let own_pid = std::process::id() as i32;
    OWN_PID.store(own_pid, Ordering::Relaxed);

    std::thread::Builder::new()
        .name("furo-focus-tracker".into())
        .spawn(move || {
            log::info!("Focus tracker started (own pid={}).", own_pid);
            loop {
                thread::sleep(Duration::from_millis(200));
                let _ = std::panic::catch_unwind(|| unsafe {
                    let pool: *mut Object = msg_send![class!(NSAutoreleasePool), new];
                    let workspace: *mut Object =
                        msg_send![class!(NSWorkspace), sharedWorkspace];
                    let app: *mut Object = msg_send![workspace, frontmostApplication];
                    if !app.is_null() {
                        let pid: i32 = msg_send![app, processIdentifier];
                        if pid != own_pid && pid > 0 {
                            LAST_EXT_PID.store(pid, Ordering::Relaxed);
                        }
                    }
                    let _: () = msg_send![pool, drain];
                });
            }
        })
        .ok();
}

/// Capture the target (frontmost external app) for text injection.
pub fn capture_target() -> Option<CapturedTarget> {
    let own_pid = OWN_PID.load(Ordering::Relaxed);

    // Wrap in catch_unwind: with the objc `exception` feature, ObjC exceptions
    // from NSWorkspace calls become Rust panics. Return None gracefully.
    let raw_pid: i32 = std::panic::catch_unwind(|| unsafe {
        let pool: *mut Object = msg_send![class!(NSAutoreleasePool), new];
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut Object = msg_send![workspace, frontmostApplication];
        let pid: i32 = if !app.is_null() {
            msg_send![app, processIdentifier]
        } else {
            -1i32
        };
        let _: () = msg_send![pool, drain];
        pid
    })
    .unwrap_or_else(|_| {
        log::warn!("[typer] ObjC exception in capture_target");
        -1
    });

    if raw_pid <= 0 {
        return None;
    }

    let target_pid = if raw_pid == own_pid {
        // Furo is foreground — use the last tracked external app.
        let last = LAST_EXT_PID.load(Ordering::Relaxed);
        if last == 0 {
            return None;
        }
        last
    } else {
        LAST_EXT_PID.store(raw_pid, Ordering::Relaxed);
        raw_pid
    };

    log::debug!("Captured target: pid={}", target_pid);
    Some(CapturedTarget {
        parent: target_pid as isize,
        child: target_pid as isize,
    })
}

/// Simulate Cmd+V via CoreGraphics keyboard events.
///
/// Posts to the HID event stream (`kCGHIDEventTap`) which delivers to
/// whichever app currently has keyboard focus. Since the Furo widget is
/// a non-activating panel, the user's target app retains focus — so the
/// paste goes to the right place without any explicit focus manipulation.
///
/// We deliberately do NOT use `CGEventPostToPid` because AppKit in many
/// apps silently drops keyboard events delivered to a process's Mach port
/// when the process's key window isn't the system's key window.
fn send_cmd_v() {
    unsafe {
        // Cmd down
        let cmd_down = CGEventCreateKeyboardEvent(std::ptr::null(), KVK_COMMAND, true);
        CGEventSetFlags(cmd_down, KCGEVENT_FLAG_MASK_COMMAND);
        CGEventPost(KCG_HID_EVENT_TAP, cmd_down);
        CFRelease(cmd_down);

        // V down (with Cmd flag)
        let v_down = CGEventCreateKeyboardEvent(std::ptr::null(), KVK_ANSI_V, true);
        CGEventSetFlags(v_down, KCGEVENT_FLAG_MASK_COMMAND);
        CGEventPost(KCG_HID_EVENT_TAP, v_down);
        CFRelease(v_down);

        // V up
        let v_up = CGEventCreateKeyboardEvent(std::ptr::null(), KVK_ANSI_V, false);
        CGEventPost(KCG_HID_EVENT_TAP, v_up);
        CFRelease(v_up);

        // Cmd up
        let cmd_up = CGEventCreateKeyboardEvent(std::ptr::null(), KVK_COMMAND, false);
        CGEventPost(KCG_HID_EVENT_TAP, cmd_up);
        CFRelease(cmd_up);
    }
}

/// Inject text into the target app via clipboard + Cmd+V.
///
/// Saves and restores the previous clipboard content so dictation
/// doesn't pollute the user's clipboard.
pub fn type_text(text: &str, target: &CapturedTarget) -> bool {
    if text.is_empty() {
        return true;
    }

    let mut output = text.to_string();
    if config::INSERT_TRAILING_SPACE {
        output.push(' ');
    }

    // ── Step 1: save clipboard ──────────────────────────────────────────
    log::info!("[typer] step 1/5 — saving clipboard via pbpaste (pid={})", target.parent);
    let prev_clipboard: Option<String> = std::process::Command::new("pbpaste")
        .output()
        .ok()
        .and_then(|o| if o.status.success() { String::from_utf8(o.stdout).ok() } else { None });
    log::info!("[typer] step 1/5 done — prev clipboard {} bytes",
        prev_clipboard.as_deref().map(|s| s.len()).unwrap_or(0));

    // ── Step 2: write transcription to clipboard ─────────────────────────
    log::info!("[typer] step 2/5 — writing {:?} to clipboard via pbcopy", output);
    {
        use std::io::Write;
        let mut child = match std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                log::error!("[typer] step 2/5 FAILED — pbcopy spawn error: {}", e);
                return false;
            }
        };
        if let Some(ref mut stdin) = child.stdin {
            if let Err(e) = stdin.write_all(output.as_bytes()) {
                log::error!("[typer] step 2/5 FAILED — pbcopy write error: {}", e);
                return false;
            }
        }
        if let Err(e) = child.wait() {
            log::error!("[typer] step 2/5 FAILED — pbcopy wait error: {}", e);
            return false;
        }
    }
    log::info!("[typer] step 2/5 done");

    // ── Step 3: validate the target process still exists ─────────────────
    // Use kill(pid, 0) — a pure POSIX syscall with no ObjC — to check liveness.
    // kill(pid, 0) returns 0 if the process exists and we have permission to
    // signal it, or errno ESRCH if it doesn't exist.
    let target_pid = target.parent as i32;
    log::info!("[typer] step 3/5 — validating pid {} via kill(0)", target_pid);
    let pid_alive = unsafe { libc::kill(target_pid, 0) } == 0;
    if !pid_alive {
        log::warn!("[typer] step 3/5 — pid {} no longer running (ESRCH), aborting", target_pid);
        return false;
    }
    log::info!("[typer] step 3/5 done — pid {} alive", target_pid);

    // ── Step 4: focus check ─────────────────────────────────────────────────
    // The Furo widget is a non-activating panel (NSWindowStyleMaskNonactivatingPanel),
    // so opening the widget never steals focus from the user's target app.
    // We no longer call activateIgnoringOtherApps:YES (it throws NSException
    // on macOS 15 Sequoia). The target app should still be focused.
    log::info!("[typer] step 4/5 — focus not changed (non-activating widget)");

    // ── Step 5: send Cmd+V then restore clipboard ─────────────────────────
    log::info!("[typer] step 5/5 — waiting {}ms then sending Cmd+V (focused app receives)",
        config::TYPING_FOCUS_DELAY_MS);
    thread::sleep(Duration::from_millis(config::TYPING_FOCUS_DELAY_MS));
    send_cmd_v();
    log::info!("[typer] step 5/5 — Cmd+V sent to HID event stream");

    // Restore the previous clipboard content.
    thread::sleep(Duration::from_millis(200));
    {
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                match prev_clipboard {
                    Some(ref prev) => { let _ = stdin.write_all(prev.as_bytes()); }
                    None => { let _ = stdin.write_all(b""); }
                }
            }
            let _ = child.wait();
        }
    }
    log::info!("[typer] type_text complete");

    true
}
