//! Windows text injection — Win32 SendInput Unicode text with verified clipboard fallback.

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use super::CapturedTarget;

use windows::Win32::Foundation::{BOOL, HMODULE, HWND};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{
    SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, SetFocus, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    KEYEVENTF_UNICODE, VK_CONTROL, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, GetForegroundWindow, GetMessageW,
    GetGUIThreadInfo, GetWindowThreadProcessId, IsIconic, IsWindow, SetForegroundWindow,
    ShowWindow, ASFW_ANY, GUITHREADINFO, MSG, SW_RESTORE,
    EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

use crate::config;

static CLIPBOARD_INJECTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
const CLIPBOARD_VERIFY_RETRIES: usize = 5;
const CLIPBOARD_VERIFY_DELAY_MS: u64 = 20;
const CLIPBOARD_RESTORE_DELAY_MS: u64 = 180;
const UNICODE_INPUT_CHUNK: usize = 64;

// ── Focus tracker globals ─────────────────────────────────────────────────
/// Our own process ID — set once in `start_focus_tracker`. 0 = not yet set.
static OWN_PID: AtomicU32 = AtomicU32::new(0);
/// Raw handle value of the last external foreground window that had a real
/// focused child control. 0 = none observed yet.
static LAST_EXT_HWND: AtomicIsize = AtomicIsize::new(0);

/// WinEvent callback: called (out-of-context, in our message-pump thread)
/// whenever the foreground window changes on the desktop.
unsafe extern "system" fn on_foreground_changed(
    _: HWINEVENTHOOK, _: u32, hwnd: HWND,
    _: i32, _: i32, _: u32, _: u32,
) {
    if hwnd.0.is_null() {
        return;
    }
    // Skip events from our own process.
    let mut pid: u32 = 0;
    let tid = GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
    if pid == 0 || pid == OWN_PID.load(Ordering::Relaxed) {
        return;
    }
    // Only update if the window actually has a focused child control
    // (avoids capturing desktop / taskbar / empty windows).
    let mut gti = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if GetGUIThreadInfo(tid, &mut gti).is_ok()
        && !gti.hwndFocus.0.is_null()
        && gti.hwndFocus.0 != hwnd.0
    {
        LAST_EXT_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
        log::debug!("Focus tracker: stored ext hwnd=0x{:X}", hwnd.0 as isize);
    }
}

/// Spawn a background thread that watches for foreground-window changes so we
/// always know the last external window with a real text-input focus — even
/// before the user presses the dictation hotkey.
pub fn start_focus_tracker() {
    let own = unsafe { GetCurrentProcessId() };
    OWN_PID.store(own, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("furo-focus-tracker".into())
        .spawn(move || unsafe {
            let hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                HMODULE(std::ptr::null_mut()),
                Some(on_foreground_changed),
                0, 0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            log::info!("Focus tracker installed (own pid={}).", own);
            // Message pump — WinEvent callbacks are delivered here.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
            if !hook.0.is_null() {
                let _ = UnhookWinEvent(hook);
            }
        })
        .ok();
}

/// Capture the target window for text injection.
///
/// Prefers the proactively-tracked last external window (recorded by
/// `start_focus_tracker`) so that the correct target is remembered even
/// if the user moved focus away before pressing the hotkey.  Falls back
/// to the live foreground window when no tracked target is available.
pub fn capture_target() -> Option<CapturedTarget> {
    unsafe {
        let fg = GetForegroundWindow();
        let own_pid = OWN_PID.load(Ordering::Relaxed);

        // Decide which top-level window to target.
        let target = if !fg.0.is_null() && own_pid != 0 {
            let mut fg_pid: u32 = 0;
            GetWindowThreadProcessId(fg, Some(&mut fg_pid as *mut u32));
            if fg_pid == own_pid {
                // Furo itself is foreground — use the last tracked external window.
                let last = LAST_EXT_HWND.load(Ordering::Relaxed);
                if last != 0 {
                    HWND(last as *mut _)
                } else {
                    return None; // Nothing useful to type into.
                }
            } else {
                // An external app is foreground — also update the tracker.
                LAST_EXT_HWND.store(fg.0 as isize, Ordering::Relaxed);
                fg
            }
        } else {
            if fg.0.is_null() { return None; }
            fg
        };

        let tid = GetWindowThreadProcessId(target, None);
        if tid == 0 {
            return Some(CapturedTarget {
                parent: target.0 as isize,
                child: target.0 as isize,
            });
        }

        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let child = if GetGUIThreadInfo(tid, &mut gti).is_ok() && !gti.hwndFocus.0.is_null() {
            gti.hwndFocus.0 as isize
        } else {
            target.0 as isize
        };

        log::debug!(
            "Captured target: parent=0x{:X}, child=0x{:X}",
            target.0 as isize,
            child
        );

        Some(CapturedTarget {
            parent: target.0 as isize,
            child,
        })
    }
}

fn make_key_input(vk: VIRTUAL_KEY, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(flags),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn make_unicode_input(code_unit: u16, key_up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE.0;
    if key_up {
        flags |= KEYEVENTF_KEYUP.0;
    }

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: code_unit,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(flags),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn target_is_foreground(target_hwnd: isize) -> bool {
    unsafe { GetForegroundWindow().0 == HWND(target_hwnd as *mut _).0 }
}

fn send_unicode_text(text: &str, target_hwnd: isize) -> bool {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    if utf16.is_empty() {
        return true;
    }

    for chunk in utf16.chunks(UNICODE_INPUT_CHUNK) {
        if !target_is_foreground(target_hwnd) {
            log::warn!("Target changed during Unicode text injection; aborting remaining text.");
            return false;
        }

        let mut inputs = Vec::with_capacity(chunk.len() * 2);
        for &code_unit in chunk {
            inputs.push(make_unicode_input(code_unit, false));
            inputs.push(make_unicode_input(code_unit, true));
        }

        let expected = inputs.len() as u32;
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent != expected {
            log::warn!(
                "Unicode SendInput sent {} events instead of {}.",
                sent,
                expected
            );
            return false;
        }
    }

    true
}

/// Simulate physical Ctrl+V via SendInput (hardware-level).
fn send_ctrl_v() {
    let inputs = [
        make_key_input(VK_CONTROL, 0),                          // Ctrl down
        make_key_input(VK_V, 0),                                // V down
        make_key_input(VK_V, KEYEVENTF_KEYUP.0),               // V up
        make_key_input(VK_CONTROL, KEYEVENTF_KEYUP.0),         // Ctrl up
    ];

    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent != 4 {
            log::warn!("SendInput returned {} instead of 4.", sent);
        }
    }
}

fn read_clipboard_text() -> Option<String> {
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.get_text().ok())
}

fn set_clipboard_text(text: &str) -> bool {
    for attempt in 0..CLIPBOARD_VERIFY_RETRIES {
        match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                if let Err(e) = clipboard.set_text(text) {
                    log::warn!("Clipboard set_text failed on attempt {}: {}", attempt + 1, e);
                } else if read_clipboard_text().as_deref() == Some(text) {
                    return true;
                } else {
                    log::warn!("Clipboard verification failed on attempt {}.", attempt + 1);
                }
            }
            Err(e) => {
                log::warn!("Failed to open clipboard on attempt {}: {}", attempt + 1, e);
            }
        }
        thread::sleep(Duration::from_millis(CLIPBOARD_VERIFY_DELAY_MS));
    }
    false
}

fn restore_clipboard_if_unchanged(expected: &str, previous: Option<String>) {
    if read_clipboard_text().as_deref() != Some(expected) {
        log::debug!("Clipboard changed after paste; preserving user's newer clipboard content.");
        return;
    }

    if let Ok(mut cb) = arboard::Clipboard::new() {
        match previous {
            Some(prev) => {
                let _ = cb.set_text(prev);
            }
            None => {
                let _ = cb.clear();
            }
        }
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

/// Bring the target window to the foreground using AttachThreadInput.
///
/// Returns `true` on success, `false` if the window is gone.
fn restore_focus(target_hwnd: isize) -> bool {
    unsafe {
        let target = HWND(target_hwnd as *mut _);
        if !IsWindow(target).as_bool() {
            return false;
        }

        // ── Fast path ───────────────────────────────────────────────────────
        // If the target window IS already the foreground, do NOT touch focus.
        //
        // Calling SetForegroundWindow / SetFocus on a window that is already
        // active sends it an unexpected WM_SETFOCUS.  Browsers like Chrome
        // handle that message by re-initialising their internal focus routing,
        // which can move focus to the address bar or another chrome widget
        // instead of preserving the text field the user was typing in.
        //
        // When the Furo widget never steals focus (WS_EX_NOACTIVATE + no
        // ShowWindow(SW_SHOW)), the target app is continuously foreground and
        // its text-field caret is intact.  SendInput → Ctrl+V will paste
        // directly into the focused field without any explicit focus juggling.
        let fg_hwnd = GetForegroundWindow();
        if fg_hwnd.0 == target.0 {
            log::debug!(
                "restore_focus: 0x{:X} is already foreground — skipping focus ops.",
                target_hwnd
            );
            return true;
        }

        // ── Normal path — target is NOT the current foreground ──────────────
        // Something else (another app, or the widget itself via a programmatic
        // SetWindowPos) stole focus.  Bring the target back.
        let our_tid = GetCurrentThreadId();
        let fg_tid = if !fg_hwnd.0.is_null() {
            GetWindowThreadProcessId(fg_hwnd, None)
        } else {
            0
        };
        let target_tid = GetWindowThreadProcessId(target, None);

        let mut attached_fg = false;
        let mut attached_target = false;

        // Attach to current foreground thread
        if fg_tid != 0 && fg_tid != our_tid {
            attached_fg = AttachThreadInput(our_tid, fg_tid, BOOL::from(true)).as_bool();
        }

        // Attach to target thread
        if target_tid != 0 && target_tid != our_tid && target_tid != fg_tid {
            attached_target = AttachThreadInput(our_tid, target_tid, BOOL::from(true)).as_bool();
        }

        // Un-minimize if needed
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
            thread::sleep(Duration::from_millis(50));
        }

        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let _ = SetForegroundWindow(target);
        let _ = BringWindowToTop(target);
        let _ = SetFocus(target);

        // ALWAYS detach — never leak thread attachments
        if attached_fg {
            let _ = AttachThreadInput(our_tid, fg_tid, BOOL::from(false));
        }
        if attached_target {
            let _ = AttachThreadInput(our_tid, target_tid, BOOL::from(false));
        }

        true
    }
}

fn type_text_via_clipboard(output: &str) -> bool {
    let prev_clipboard = read_clipboard_text();

    if !set_clipboard_text(output) {
        log::warn!("Could not place transcription on clipboard after retries.");
        return false;
    }

    if read_clipboard_text().as_deref() != Some(output) {
        log::warn!("Clipboard changed before paste; aborting fallback paste.");
        restore_clipboard_if_unchanged(output, prev_clipboard);
        return false;
    }

    send_ctrl_v();
    schedule_clipboard_restore(output.to_string(), prev_clipboard);
    true
}

/// Inject text into the target window.
///
/// Primary path uses SendInput Unicode packets, which does not touch the system
/// clipboard. The clipboard path is only a verified fallback for apps that reject
/// Unicode packets.
pub fn type_text(text: &str, target: &CapturedTarget) -> bool {
    if text.is_empty() {
        return true;
    }

    let mut output = text.to_string();
    if config::INSERT_TRAILING_SPACE {
        output.push(' ');
    }

    unsafe {
        let parent = HWND(target.parent as *mut _);
        if !IsWindow(parent).as_bool() {
            log::warn!(
                "Parent HWND 0x{:X} destroyed — aborting text injection.",
                target.parent
            );
            return false;
        }
    }

    if !restore_focus(target.parent) {
        log::warn!(
            "Focus restore failed for 0x{:X} — aborting text injection.",
            target.parent
        );
        return false;
    }

    thread::sleep(Duration::from_millis(config::TYPING_FOCUS_DELAY_MS));

    if !target_is_foreground(target.parent) {
        log::warn!(
            "Target HWND 0x{:X} is no longer foreground — aborting text injection.",
            target.parent
        );
        return false;
    }

    if send_unicode_text(&output, target.parent) {
        log::debug!("Unicode text sent to 0x{:X} via SendInput.", target.parent);
        return true;
    }

    let _clipboard_guard = CLIPBOARD_INJECTION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !target_is_foreground(target.parent) {
        log::warn!("Target changed before clipboard fallback — aborting paste.");
        return false;
    }
    type_text_via_clipboard(&output)
}
