//! macOS text injection — CGEvent Unicode text with Cmd+V clipboard fallback.
//!
//! Uses:
//!   - NSWorkspace to track the frontmost application
//!   - NSRunningApplication to restore focus
//!   - CoreGraphics CGEvent to inject Unicode text without touching the clipboard
//!   - pbcopy/pbpaste only for verified fallback paste
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

static CLIPBOARD_INJECTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
const CLIPBOARD_VERIFY_RETRIES: usize = 5;
const CLIPBOARD_VERIFY_DELAY_MS: u64 = 20;
const CLIPBOARD_RESTORE_DELAY_MS: u64 = 180;

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
    fn CGEventKeyboardSetUnicodeString(
        event: *mut c_void,
        string_length: usize,
        unicode_string: *const u16,
    );
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
                thread::sleep(Duration::from_millis(100));
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

fn frontmost_pid() -> Option<i32> {
    std::panic::catch_unwind(|| unsafe {
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
    .ok()
    .filter(|pid| *pid > 0)
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

fn send_unicode_text(text: &str) -> bool {
    for character in text.chars() {
        let mut utf16 = [0u16; 2];
        let units = character.encode_utf16(&mut utf16);

        unsafe {
            let key_down = CGEventCreateKeyboardEvent(std::ptr::null(), 0, true);
            if key_down.is_null() {
                log::warn!("[typer] CGEventCreateKeyboardEvent returned null for unicode key down");
                return false;
            }
            CGEventKeyboardSetUnicodeString(key_down, units.len(), units.as_ptr());
            CGEventPost(KCG_HID_EVENT_TAP, key_down);
            CFRelease(key_down);

            let key_up = CGEventCreateKeyboardEvent(std::ptr::null(), 0, false);
            if key_up.is_null() {
                log::warn!("[typer] CGEventCreateKeyboardEvent returned null for unicode key up");
                return false;
            }
            CGEventPost(KCG_HID_EVENT_TAP, key_up);
            CFRelease(key_up);
        }
    }

    true
}

fn read_clipboard_text() -> Option<String> {
    std::process::Command::new("pbpaste")
        .output()
        .ok()
        .and_then(|o| if o.status.success() { String::from_utf8(o.stdout).ok() } else { None })
}

fn write_clipboard_text(text: &str) -> bool {
    use std::io::Write;

    for attempt in 0..CLIPBOARD_VERIFY_RETRIES {
        let mut child = match std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[typer] pbcopy spawn failed on attempt {}: {}", attempt + 1, e);
                thread::sleep(Duration::from_millis(CLIPBOARD_VERIFY_DELAY_MS));
                continue;
            }
        };

        if let Some(ref mut stdin) = child.stdin {
            if let Err(e) = stdin.write_all(text.as_bytes()) {
                log::warn!("[typer] pbcopy write failed on attempt {}: {}", attempt + 1, e);
            }
        }

        match child.wait() {
            Ok(status) if status.success() && read_clipboard_text().as_deref() == Some(text) => {
                return true;
            }
            Ok(status) => {
                log::warn!("[typer] pbcopy verification failed on attempt {}: {}", attempt + 1, status);
            }
            Err(e) => {
                log::warn!("[typer] pbcopy wait failed on attempt {}: {}", attempt + 1, e);
            }
        }

        thread::sleep(Duration::from_millis(CLIPBOARD_VERIFY_DELAY_MS));
    }

    false
}

fn restore_clipboard_if_unchanged(expected: &str, previous: Option<String>) {
    if read_clipboard_text().as_deref() != Some(expected) {
        log::debug!("[typer] clipboard changed after paste; preserving newer clipboard content");
        return;
    }

    let restore_text = previous.unwrap_or_default();
    if !write_clipboard_text(&restore_text) {
        log::warn!("[typer] failed to restore clipboard after paste");
    }
}

fn schedule_clipboard_restore(expected: String, previous: Option<String>) {
    let _ = thread::Builder::new()
        .name("furo-clipboard-restore".into())
        .spawn(move || {
            thread::sleep(Duration::from_millis(CLIPBOARD_RESTORE_DELAY_MS));
            let _clipboard_guard = CLIPBOARD_INJECTION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            restore_clipboard_if_unchanged(&expected, previous);
        });
}

fn type_text_via_clipboard(output: &str) -> bool {
    let prev_clipboard = read_clipboard_text();

    if !write_clipboard_text(output) {
        log::error!("[typer] fallback failed — could not verify pbcopy write");
        return false;
    }

    if read_clipboard_text().as_deref() != Some(output) {
        log::warn!("[typer] clipboard changed before paste; aborting fallback paste");
        restore_clipboard_if_unchanged(output, prev_clipboard);
        return false;
    }

    send_cmd_v();
    schedule_clipboard_restore(output.to_string(), prev_clipboard);
    true
}

/// Inject text into the target app.
///
/// Primary path posts Unicode keyboard events and does not touch the general
/// pasteboard. Cmd+V is retained only as a verified fallback.
pub fn type_text(text: &str, target: &CapturedTarget) -> bool {
    if text.is_empty() {
        return true;
    }

    let mut output = text.to_string();
    if config::INSERT_TRAILING_SPACE {
        output.push(' ');
    }

    // Use kill(pid, 0) — a pure POSIX syscall with no ObjC — to check liveness.
    // kill(pid, 0) returns 0 if the process exists and we have permission to
    // signal it, or errno ESRCH if it doesn't exist.
    let target_pid = target.parent as i32;
    let pid_alive = unsafe { libc::kill(target_pid, 0) } == 0;
    if !pid_alive {
        log::warn!("[typer] pid {} no longer running (ESRCH), aborting", target_pid);
        return false;
    }

    if frontmost_pid() != Some(target_pid) {
        log::warn!("[typer] target pid {} is no longer frontmost; aborting", target_pid);
        return false;
    }

    // The Furo widget is a non-activating panel (NSWindowStyleMaskNonactivatingPanel),
    // so opening the widget never steals focus from the user's target app.
    // We no longer call activateIgnoringOtherApps:YES (it throws NSException
    // on macOS 15 Sequoia). The target app should still be focused.
    thread::sleep(Duration::from_millis(config::TYPING_FOCUS_DELAY_MS));

    if send_unicode_text(&output) {
        log::info!("[typer] unicode text sent to HID event stream");
        return true;
    }

    let _clipboard_guard = CLIPBOARD_INJECTION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    type_text_via_clipboard(&output)
}
