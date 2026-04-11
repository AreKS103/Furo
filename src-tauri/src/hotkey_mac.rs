//! macOS global hotkey system — CGEvent tap.
//!
//! Uses a Quartz Event Tap to intercept key and mouse events system-wide.
//! Requires Accessibility permissions (System Preferences > Privacy & Security > Accessibility).

use crossbeam_channel::Sender;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use super::{
    hotkey_worker, parse_hotkey_combo, HotkeyCallbacks, HotkeyEvent, MouseButton,
};

// ── CoreGraphics FFI ──────────────────────────────────────────────────────

type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CGEventType = u32;
type CGEventMask = u64;
type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;

const KCG_EVENT_KEY_DOWN: CGEventType = 10;
const KCG_EVENT_KEY_UP: CGEventType = 11;
const KCG_EVENT_FLAGS_CHANGED: CGEventType = 12;
const KCG_EVENT_LEFT_MOUSE_DOWN: CGEventType = 1;
const KCG_EVENT_LEFT_MOUSE_UP: CGEventType = 2;
const KCG_EVENT_RIGHT_MOUSE_DOWN: CGEventType = 3;
const KCG_EVENT_RIGHT_MOUSE_UP: CGEventType = 4;
const KCG_EVENT_OTHER_MOUSE_DOWN: CGEventType = 25;
const KCG_EVENT_OTHER_MOUSE_UP: CGEventType = 26;

const KCG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const KCG_SESSION_EVENT_TAP: u32 = 1;

// CGEventField for keycode
const KCG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
// CGEventField for mouse button number
const KCG_MOUSE_EVENT_BUTTON_NUMBER: u32 = 3;

// CGEvent modifier flags
const KCG_EVENT_FLAG_MASK_SHIFT: u64 = 1 << 17;
const KCG_EVENT_FLAG_MASK_CONTROL: u64 = 1 << 18;
const KCG_EVENT_FLAG_MASK_ALTERNATE: u64 = 1 << 19; // Option/Alt
const KCG_EVENT_FLAG_MASK_COMMAND: u64 = 1 << 20;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: extern "C" fn(CGEventTapProxy, CGEventType, CGEventRef, *mut c_void) -> CGEventRef,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: i64,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: *const c_void);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRelease(cf: *const c_void);

    // kCFRunLoopCommonModes is a global CFStringRef
    static kCFRunLoopCommonModes: *const c_void;
}

// ── macOS keycode → Windows VK mapping ────────────────────────────────────

/// Convert a macOS virtual keycode to the canonical Windows VK code used by
/// our hotkey combo parser. This keeps the worker thread platform-agnostic.
fn mac_keycode_to_vk(keycode: u16) -> u32 {
    match keycode {
        0x00 => 0x41, // A
        0x01 => 0x53, // S
        0x02 => 0x44, // D
        0x03 => 0x46, // F
        0x04 => 0x48, // H
        0x05 => 0x47, // G
        0x06 => 0x5A, // Z
        0x07 => 0x58, // X
        0x08 => 0x43, // C
        0x09 => 0x56, // V
        0x0B => 0x42, // B
        0x0C => 0x51, // Q
        0x0D => 0x57, // W
        0x0E => 0x45, // E
        0x0F => 0x52, // R
        0x10 => 0x59, // Y
        0x11 => 0x54, // T
        0x12 => 0x31, // 1
        0x13 => 0x32, // 2
        0x14 => 0x33, // 3
        0x15 => 0x34, // 4
        0x16 => 0x36, // 6
        0x17 => 0x35, // 5
        0x18 => 0xBB, // = (VK_OEM_PLUS)
        0x19 => 0x39, // 9
        0x1A => 0x37, // 7
        0x1B => 0xBD, // - (VK_OEM_MINUS)
        0x1C => 0x38, // 8
        0x1D => 0x30, // 0
        0x1E => 0xDD, // ] (VK_OEM_6)
        0x1F => 0x4F, // O
        0x20 => 0x55, // U
        0x21 => 0xDB, // [ (VK_OEM_4)
        0x22 => 0x49, // I
        0x23 => 0x50, // P
        0x24 => 0x0D, // Return → VK_RETURN
        0x25 => 0x4C, // L
        0x26 => 0x4A, // J
        0x27 => 0xDE, // ' (VK_OEM_7)
        0x28 => 0x4B, // K
        0x29 => 0xBA, // ; (VK_OEM_1)
        0x2A => 0xDC, // \ (VK_OEM_5)
        0x2B => 0xBC, // , (VK_OEM_COMMA)
        0x2C => 0xBF, // / (VK_OEM_2)
        0x2D => 0x4E, // N
        0x2E => 0x4D, // M
        0x2F => 0xBE, // . (VK_OEM_PERIOD)
        0x30 => 0x09, // Tab → VK_TAB
        0x31 => 0x20, // Space → VK_SPACE
        0x32 => 0xC0, // ` (VK_OEM_3)
        0x33 => 0x08, // Delete (Backspace) → VK_BACK
        0x35 => 0x1B, // Escape → VK_ESCAPE
        0x37 => 0x5B, // Command → VK_LWIN
        0x38 => 0x10, // Shift → VK_SHIFT
        0x39 => 0x14, // CapsLock → VK_CAPITAL
        0x3A => 0x12, // Option → VK_MENU (Alt)
        0x3B => 0x11, // Control → VK_CONTROL
        0x3C => 0x10, // Right Shift
        0x3D => 0x12, // Right Option
        0x3E => 0x11, // Right Control
        0x36 => 0x5C, // Right Command → VK_RWIN
        // Function keys
        0x7A => 0x70, // F1
        0x78 => 0x71, // F2
        0x63 => 0x72, // F3
        0x76 => 0x73, // F4
        0x60 => 0x74, // F5
        0x61 => 0x75, // F6
        0x62 => 0x76, // F7
        0x64 => 0x77, // F8
        0x65 => 0x78, // F9
        0x6D => 0x79, // F10
        0x67 => 0x7A, // F11
        0x6F => 0x7B, // F12
        // Arrow keys
        0x7B => 0x25, // Left
        0x7C => 0x27, // Right
        0x7D => 0x28, // Down
        0x7E => 0x26, // Up
        // Navigation
        0x73 => 0x24, // Home
        0x77 => 0x23, // End
        0x74 => 0x21, // PageUp
        0x79 => 0x22, // PageDown
        0x75 => 0x2E, // Forward Delete
        _ => 0, // Unknown — ignored
    }
}

// ── Global sender for the event tap callback ──────────────────────────────

static HOOK_SENDER: std::sync::Mutex<Option<Sender<HotkeyEvent>>> = std::sync::Mutex::new(None);

/// Previous modifier flags — used to detect modifier-only press/release events.
static PREV_FLAGS: AtomicU64 = AtomicU64::new(0);

/// CGEvent tap callback — translates macOS events into `HotkeyEvent`s.
extern "C" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    let sender_guard = HOOK_SENDER.lock().unwrap();
    let Some(sender) = sender_guard.as_ref() else {
        return event;
    };

    match event_type {
        KCG_EVENT_KEY_DOWN => {
            let keycode = unsafe { CGEventGetIntegerValueField(event, KCG_KEYBOARD_EVENT_KEYCODE) } as u16;
            let vk = mac_keycode_to_vk(keycode);
            if vk != 0 {
                let _ = sender.try_send(HotkeyEvent::KeyPress { vk, injected: false });
            }
        }
        KCG_EVENT_KEY_UP => {
            let keycode = unsafe { CGEventGetIntegerValueField(event, KCG_KEYBOARD_EVENT_KEYCODE) } as u16;
            let vk = mac_keycode_to_vk(keycode);
            if vk != 0 {
                let _ = sender.try_send(HotkeyEvent::KeyRelease { vk, injected: false });
            }
        }
        KCG_EVENT_FLAGS_CHANGED => {
            // Modifier-only events: detect press vs release by comparing with previous flags.
            let flags = unsafe { CGEventGetFlags(event) };
            let prev = PREV_FLAGS.swap(flags, Ordering::SeqCst);
            let keycode = unsafe { CGEventGetIntegerValueField(event, KCG_KEYBOARD_EVENT_KEYCODE) } as u16;
            let vk = mac_keycode_to_vk(keycode);
            if vk != 0 {
                // Check the specific modifier bit to determine press vs release.
                let mask = match keycode {
                    0x37 | 0x36 => KCG_EVENT_FLAG_MASK_COMMAND,
                    0x38 | 0x3C => KCG_EVENT_FLAG_MASK_SHIFT,
                    0x3A | 0x3D => KCG_EVENT_FLAG_MASK_ALTERNATE,
                    0x3B | 0x3E => KCG_EVENT_FLAG_MASK_CONTROL,
                    _ => 0,
                };
                if mask != 0 {
                    let was_down = (prev & mask) != 0;
                    let is_down = (flags & mask) != 0;
                    if is_down && !was_down {
                        let _ = sender.try_send(HotkeyEvent::KeyPress { vk, injected: false });
                    } else if !is_down && was_down {
                        let _ = sender.try_send(HotkeyEvent::KeyRelease { vk, injected: false });
                    }
                }
            }
        }
        KCG_EVENT_LEFT_MOUSE_DOWN => {
            let _ = sender.try_send(HotkeyEvent::MousePress { button: MouseButton::Left });
        }
        KCG_EVENT_LEFT_MOUSE_UP => {
            let _ = sender.try_send(HotkeyEvent::MouseRelease { button: MouseButton::Left });
        }
        KCG_EVENT_RIGHT_MOUSE_DOWN => {
            let _ = sender.try_send(HotkeyEvent::MousePress { button: MouseButton::Right });
        }
        KCG_EVENT_RIGHT_MOUSE_UP => {
            let _ = sender.try_send(HotkeyEvent::MouseRelease { button: MouseButton::Right });
        }
        KCG_EVENT_OTHER_MOUSE_DOWN => {
            let btn_num = unsafe { CGEventGetIntegerValueField(event, KCG_MOUSE_EVENT_BUTTON_NUMBER) };
            let button = match btn_num {
                2 => MouseButton::Middle,
                3 => MouseButton::X1,
                4 => MouseButton::X2,
                _ => return event,
            };
            let _ = sender.try_send(HotkeyEvent::MousePress { button });
        }
        KCG_EVENT_OTHER_MOUSE_UP => {
            let btn_num = unsafe { CGEventGetIntegerValueField(event, KCG_MOUSE_EVENT_BUTTON_NUMBER) };
            let button = match btn_num {
                2 => MouseButton::Middle,
                3 => MouseButton::X1,
                4 => MouseButton::X2,
                _ => return event,
            };
            let _ = sender.try_send(HotkeyEvent::MouseRelease { button });
        }
        _ => {}
    }

    event
}

// ── RunLoop handle for stopping the tap ───────────────────────────────────

static TAP_RUNLOOP: std::sync::Mutex<Option<SendPtr>> = std::sync::Mutex::new(None);

#[derive(Clone, Copy)]
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

// ============================================================================
// Public API — matches HotkeyListener from hotkey_win.rs
// ============================================================================

pub struct HotkeyListener {
    stop_flag: Arc<AtomicBool>,
    tap_thread: Option<thread::JoinHandle<()>>,
    worker_thread: Option<thread::JoinHandle<()>>,
}

impl HotkeyListener {
    pub fn start(
        hold_combo_str: &str,
        handsfree_combo_str: &str,
        callbacks: HotkeyCallbacks,
    ) -> Self {
        let hold_combo = parse_hotkey_combo(hold_combo_str);
        let handsfree_combo = parse_hotkey_combo(handsfree_combo_str);

        let (sender, receiver) = crossbeam_channel::bounded::<HotkeyEvent>(256);
        *HOOK_SENDER.lock().unwrap() = Some(sender);

        let stop_flag = Arc::new(AtomicBool::new(false));

        // Spawn event tap thread (equivalent to Windows hook thread)
        let tap_thread = thread::Builder::new()
            .name("furo-event-tap".into())
            .spawn(move || {
                unsafe {
                    // Build event mask for keys + modifiers + mouse buttons
                    let mask: CGEventMask = (1 << KCG_EVENT_KEY_DOWN)
                        | (1 << KCG_EVENT_KEY_UP)
                        | (1 << KCG_EVENT_FLAGS_CHANGED)
                        | (1 << KCG_EVENT_LEFT_MOUSE_DOWN)
                        | (1 << KCG_EVENT_LEFT_MOUSE_UP)
                        | (1 << KCG_EVENT_RIGHT_MOUSE_DOWN)
                        | (1 << KCG_EVENT_RIGHT_MOUSE_UP)
                        | (1 << KCG_EVENT_OTHER_MOUSE_DOWN)
                        | (1 << KCG_EVENT_OTHER_MOUSE_UP);

                    let tap = CGEventTapCreate(
                        KCG_SESSION_EVENT_TAP,
                        KCG_HEAD_INSERT_EVENT_TAP,
                        0, // kCGEventTapOptionDefault (active tap)
                        mask,
                        event_tap_callback,
                        std::ptr::null_mut(),
                    );

                    if tap.is_null() {
                        log::error!(
                            "CGEventTapCreate failed — Accessibility permission not granted. \
                             Go to System Preferences > Privacy & Security > Accessibility and add Furo."
                        );
                        return;
                    }

                    let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
                    let run_loop = CFRunLoopGetCurrent();

                    // Store the RunLoop ref so stop() can break out of CFRunLoopRun.
                    *TAP_RUNLOOP.lock().unwrap() = Some(SendPtr(run_loop));

                    CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);

                    log::info!("macOS event tap installed — listening for hotkeys.");
                    CFRunLoopRun(); // Blocks until CFRunLoopStop is called.

                    // Cleanup
                    CFRelease(source);
                    CFRelease(tap);
                    *TAP_RUNLOOP.lock().unwrap() = None;
                    log::info!("macOS event tap uninstalled.");
                }
            })
            .expect("Failed to spawn event tap thread");

        // Spawn worker thread (same as Windows)
        let stop = stop_flag.clone();
        let worker_thread = thread::Builder::new()
            .name("furo-hotkey-worker".into())
            .spawn(move || {
                hotkey_worker(receiver, stop, hold_combo, handsfree_combo, callbacks);
            })
            .expect("Failed to spawn hotkey worker thread");

        log::info!(
            "Hotkey listeners started — hold: {}, hands-free: {}",
            hold_combo_str,
            handsfree_combo_str,
        );

        Self {
            stop_flag,
            tap_thread: Some(tap_thread),
            worker_thread: Some(worker_thread),
        }
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);

        // Stop the CFRunLoop to unblock the tap thread.
        if let Some(SendPtr(rl)) = *TAP_RUNLOOP.lock().unwrap() {
            unsafe { CFRunLoopStop(rl) };
        }

        if let Some(h) = self.tap_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.worker_thread.take() {
            let _ = h.join();
        }

        log::info!("Hotkey listeners stopped.");
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        if self.tap_thread.is_some() || self.worker_thread.is_some() {
            self.stop();
        }
    }
}
