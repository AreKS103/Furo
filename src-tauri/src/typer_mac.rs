//! macOS text injection — CGEvent Cmd+V + NSRunningApplication.
//!
//! Uses:
//!   - NSWorkspace to track the frontmost application
//!   - NSRunningApplication to restore focus
//!   - CoreGraphics CGEvent to simulate Cmd+V keystrokes
//!   - arboard for cross-platform clipboard access
//!
//! Requires Accessibility access (System Preferences > Privacy & Security > Accessibility).

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::Duration;

use objc::runtime::{Object, BOOL, YES};
use objc::{class, msg_send, sel, sel_impl};

use crate::config;
use super::CapturedTarget;

// ── macOS virtual key codes ───────────────────────────────────────────────
const KVK_COMMAND: u16 = 0x37;
const KVK_ANSI_V: u16 = 0x09;

// CGEvent modifier flags
const KCGEVENT_FLAG_MASK_COMMAND: u64 = 1 << 20;

// CGEventTapLocation
const KCG_HID_EVENT_TAP: u32 = 0;

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

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

// ── Focus tracker globals ─────────────────────────────────────────────────
static OWN_PID: AtomicI32 = AtomicI32::new(0);
static LAST_EXT_PID: AtomicI32 = AtomicI32::new(0);

/// Spawn a background thread that polls NSWorkspace.frontmostApplication
/// every 200 ms and remembers the last external (non-Furo) app PID.
pub fn start_focus_tracker() {
    let own_pid = std::process::id() as i32;
    OWN_PID.store(own_pid, Ordering::Relaxed);

    std::thread::Builder::new()
        .name("furo-focus-tracker".into())
        .spawn(move || unsafe {
            log::info!("Focus tracker started (own pid={}).", own_pid);
            loop {
                thread::sleep(Duration::from_millis(200));
                let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
                let app: *mut Object = msg_send![workspace, frontmostApplication];
                if !app.is_null() {
                    let pid: i32 = msg_send![app, processIdentifier];
                    if pid != own_pid && pid > 0 {
                        LAST_EXT_PID.store(pid, Ordering::Relaxed);
                    }
                }
            }
        })
        .ok();
}

/// Capture the target (frontmost external app) for text injection.
pub fn capture_target() -> Option<CapturedTarget> {
    unsafe {
        let own_pid = OWN_PID.load(Ordering::Relaxed);
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut Object = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];

        let target_pid = if pid == own_pid {
            // Furo is foreground — use the last tracked external app.
            let last = LAST_EXT_PID.load(Ordering::Relaxed);
            if last == 0 {
                return None;
            }
            last
        } else {
            LAST_EXT_PID.store(pid, Ordering::Relaxed);
            pid
        };

        log::debug!("Captured target: pid={}", target_pid);
        Some(CapturedTarget {
            parent: target_pid as isize,
            child: target_pid as isize,
        })
    }
}

/// Bring the target process to the foreground.
fn restore_focus(target_pid: i32) -> bool {
    unsafe {
        let cls = class!(NSRunningApplication);
        let app: *mut Object =
            msg_send![cls, runningApplicationWithProcessIdentifier: target_pid];
        if app.is_null() {
            log::warn!("No running app with pid {} — cannot restore focus.", target_pid);
            return false;
        }
        // activateIgnoringOtherApps: is available since macOS 10.6.
        let ok: BOOL = msg_send![app, activateIgnoringOtherApps: YES];
        ok
    }
}

/// Simulate Cmd+V via CoreGraphics keyboard events.
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

    // Save previous clipboard content.
    let prev_clipboard: Option<String> = arboard::Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.get_text().ok());

    // Put transcription text on clipboard.
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            if let Err(e) = clipboard.set_text(&output) {
                log::warn!("Clipboard set_text failed: {}", e);
                return false;
            }
        }
        Err(e) => {
            log::warn!("Failed to open clipboard: {}", e);
            return false;
        }
    }

    // Validate the process still exists.
    let target_pid = target.parent as i32;
    unsafe {
        let cls = class!(NSRunningApplication);
        let app: *mut Object =
            msg_send![cls, runningApplicationWithProcessIdentifier: target_pid];
        if app.is_null() {
            log::warn!("Process {} no longer running.", target_pid);
            return false;
        }
    }

    // Restore focus.
    if !restore_focus(target_pid) {
        log::warn!("Focus restore failed for pid {}.", target_pid);
        return false;
    }

    // Small delay for the app to settle, then Cmd+V.
    thread::sleep(Duration::from_millis(config::TYPING_FOCUS_DELAY_MS));
    send_cmd_v();
    log::debug!("Cmd+V sent to pid {}.", target_pid);

    // Restore the previous clipboard content.
    thread::sleep(Duration::from_millis(200));
    if let Ok(mut cb) = arboard::Clipboard::new() {
        match prev_clipboard {
            Some(prev) => { let _ = cb.set_text(&prev); }
            None => { let _ = cb.clear(); }
        }
    }

    true
}
