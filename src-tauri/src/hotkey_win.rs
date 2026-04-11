//! Windows global hotkey system — Win32 low-level hooks.

use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    XBUTTON1,
};

use super::{
    hotkey_worker, parse_hotkey_combo, HotkeyCallbacks, HotkeyEvent, MouseButton,
};

// ── Hook listener

/// Manages the low-level hook thread and worker thread.
pub struct HotkeyListener {
    hook_thread_id: Arc<AtomicU32>,
    stop_flag: Arc<AtomicBool>,
    hook_thread: Option<thread::JoinHandle<()>>,
    worker_thread: Option<thread::JoinHandle<()>>,
}

// Static channel sender for use in the hook callback (must be global for the C callback).
// Uses Mutex<Option<…>> instead of OnceLock so the sender can be replaced on listener restart.
static HOOK_SENDER: std::sync::Mutex<Option<Sender<HotkeyEvent>>> = std::sync::Mutex::new(None);

// SAFETY: HHOOK is a thin wrapper around *mut c_void returned by
// SetWindowsHookExW. The handle is only ever read (CallNextHookEx) or
// written (SetWindowsHookExW / UnhookWindowsHookEx) while holding the
// enclosing std::sync::Mutex, and all accesses happen within the same
// hook thread. Sending the handle across threads (via the Mutex) is safe
// because Windows hook handles are process-global and thread-agnostic.
#[derive(Debug, Clone, Copy)]
struct SendHook(HHOOK);
unsafe impl Send for SendHook {}
unsafe impl Sync for SendHook {}

static KB_HOOK: std::sync::Mutex<Option<SendHook>> = std::sync::Mutex::new(None);
static MOUSE_HOOK: std::sync::Mutex<Option<SendHook>> = std::sync::Mutex::new(None);

/// Low-level keyboard hook procedure.
unsafe extern "system" fn keyboard_hook_proc(
    n_code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if n_code >= 0 {
        let info = &*(l_param.0 as *const KBDLLHOOKSTRUCT);
        let injected = (info.flags.0 & LLKHF_INJECTED.0) != 0;
        let vk = info.vkCode;

        let event = match w_param.0 as u32 {
            WM_KEYDOWN | WM_SYSKEYDOWN => Some(HotkeyEvent::KeyPress { vk, injected }),
            WM_KEYUP | WM_SYSKEYUP => Some(HotkeyEvent::KeyRelease { vk, injected }),
            _ => None,
        };

        if let Some(evt) = event {
            if let Ok(guard) = HOOK_SENDER.lock() {
                if let Some(ref sender) = *guard {
                    let _ = sender.try_send(evt);
                }
            }

            // Suppress Win key from reaching the OS (Start menu / language switcher) when:
            //   • rebind mode is active — user is assigning a new hotkey, OR
            //   • Win is part of a registered combo — prevents language switcher on use
            const VK_LWIN: u32 = 0x5B;
            const VK_RWIN: u32 = 0x5C;
            if (vk == VK_LWIN || vk == VK_RWIN)
                && (super::REBIND_MODE_ACTIVE.load(Ordering::Relaxed)
                    || super::WIN_IS_COMBO_MODIFIER.load(Ordering::Relaxed))
            {
                return LRESULT(1); // consumed — OS never sees this keystroke
            }
        }
    }

    let hook = KB_HOOK.lock().unwrap().map(|h| h.0).unwrap_or(HHOOK::default());
    CallNextHookEx(hook, n_code, w_param, l_param)
}

/// Low-level mouse hook procedure.
unsafe extern "system" fn mouse_hook_proc(
    n_code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if n_code >= 0 {
        let info = &*(l_param.0 as *const MSLLHOOKSTRUCT);
        let msg = w_param.0 as u32;

        let event = match msg {
            WM_LBUTTONDOWN => Some(HotkeyEvent::MousePress {
                button: MouseButton::Left,
            }),
            WM_LBUTTONUP => Some(HotkeyEvent::MouseRelease {
                button: MouseButton::Left,
            }),
            WM_RBUTTONDOWN => Some(HotkeyEvent::MousePress {
                button: MouseButton::Right,
            }),
            WM_RBUTTONUP => Some(HotkeyEvent::MouseRelease {
                button: MouseButton::Right,
            }),
            WM_MBUTTONDOWN => Some(HotkeyEvent::MousePress {
                button: MouseButton::Middle,
            }),
            WM_MBUTTONUP => Some(HotkeyEvent::MouseRelease {
                button: MouseButton::Middle,
            }),
            WM_XBUTTONDOWN => {
                let x_button = ((info.mouseData >> 16) & 0xFFFF) as u16;
                let btn = if x_button == XBUTTON1 as u16 {
                    MouseButton::X1
                } else {
                    MouseButton::X2
                };
                Some(HotkeyEvent::MousePress { button: btn })
            }
            WM_XBUTTONUP => {
                let x_button = ((info.mouseData >> 16) & 0xFFFF) as u16;
                let btn = if x_button == XBUTTON1 as u16 {
                    MouseButton::X1
                } else {
                    MouseButton::X2
                };
                Some(HotkeyEvent::MouseRelease { button: btn })
            }
            _ => None,
        };

        if let Some(evt) = event {
            if let Ok(guard) = HOOK_SENDER.lock() {
                if let Some(ref sender) = *guard {
                    let _ = sender.try_send(evt);
                }
            }
        }
    }

    let hook = MOUSE_HOOK.lock().unwrap().map(|h| h.0).unwrap_or(HHOOK::default());
    CallNextHookEx(hook, n_code, w_param, l_param)
}

impl HotkeyListener {
    pub fn start(
        hold_combo_str: &str,
        handsfree_combo_str: &str,
        callbacks: HotkeyCallbacks,
    ) -> Result<Self, String> {
        let hold_combo = parse_hotkey_combo(hold_combo_str);
        let handsfree_combo = parse_hotkey_combo(handsfree_combo_str);

        let (sender, receiver) = crossbeam_channel::bounded::<HotkeyEvent>(256);

        // Store sender — replaces any previous sender from a stopped listener
        *HOOK_SENDER.lock().unwrap() = Some(sender);

        let stop_flag = Arc::new(AtomicBool::new(false));
        let hook_thread_id = Arc::new(AtomicU32::new(0));

        // Spawn hook thread
        let hook_tid = hook_thread_id.clone();
        let hook_thread = thread::Builder::new()
            .name("furo-hook".into())
            .spawn(move || {
                unsafe {
                    hook_tid.store(GetCurrentThreadId_compat(), Ordering::SeqCst);

                    let hmod = GetModuleHandleW(None)
                        .map(|m| HINSTANCE(m.0))
                        .unwrap_or(HINSTANCE::default());

                    let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), hmod, 0)
                        .expect("Failed to install keyboard hook");
                    *KB_HOOK.lock().unwrap() = Some(SendHook(kb));

                    let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), hmod, 0)
                        .expect("Failed to install mouse hook");
                    *MOUSE_HOOK.lock().unwrap() = Some(SendHook(mouse));

                    log::info!("Low-level keyboard and mouse hooks installed.");

                    // Message pump to keep hooks alive
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        // WM_QUIT breaks the loop
                    }

                    let _ = UnhookWindowsHookEx(kb);
                    let _ = UnhookWindowsHookEx(mouse);
                    *KB_HOOK.lock().unwrap() = None;
                    *MOUSE_HOOK.lock().unwrap() = None;

                    log::info!("Low-level hooks uninstalled.");
                }
            })
            .expect("Failed to spawn hook thread");

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
            handsfree_combo_str
        );

        Ok(Self {
            hook_thread_id,
            stop_flag,
            hook_thread: Some(hook_thread),
            worker_thread: Some(worker_thread),
        })
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);

        // Post WM_QUIT to the hook thread's message loop
        let tid = self.hook_thread_id.load(Ordering::SeqCst);
        if tid != 0 {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }

        if let Some(h) = self.hook_thread.take() {
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
        if self.hook_thread.is_some() || self.worker_thread.is_some() {
            self.stop();
        }
    }
}

#[allow(non_snake_case)]
fn GetCurrentThreadId_compat() -> u32 {
    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
}
