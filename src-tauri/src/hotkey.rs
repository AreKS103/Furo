//! Project Furo â€” Global Hotkey System (cross-platform)
//!
//! Shared types, combo parsing, and the platform-agnostic worker thread.
//! Platform-specific hook/listener code is in hotkey_win.rs / hotkey_mac.rs.

use crossbeam_channel::Receiver;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(target_os = "windows")]
#[path = "hotkey_win.rs"]
mod platform;

#[cfg(target_os = "macos")]
#[path = "hotkey_mac.rs"]
mod platform;

pub use platform::HotkeyListener;

// ── Rebind mode state — shared between hook proc and worker

/// Set to `true` while the UI is in rebind mode.
/// The hook suppresses Win key OS defaults; the worker captures the next combo.
pub(crate) static REBIND_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set to `true` when Win is a modifier in any registered combo.
/// Hook suppresses Win→Start-menu / Win+Space→language-switcher during hotkey use.
pub(crate) static WIN_IS_COMBO_MODIFIER: AtomicBool = AtomicBool::new(false);

// ── Hotkey event types

#[derive(Debug, Clone)]
pub enum HotkeyEvent {
    KeyPress { vk: u32, injected: bool },
    KeyRelease { vk: u32, injected: bool },
    MousePress { button: MouseButton },
    MouseRelease { button: MouseButton },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

impl MouseButton {
    pub fn name(&self) -> &'static str {
        match self {
            MouseButton::Left => "mouse1",
            MouseButton::Right => "mouse2",
            MouseButton::Middle => "mouse3",
            MouseButton::X1 => "mouse4",
            MouseButton::X2 => "mouse5",
        }
    }
}

// ── Parsed hotkey combo

#[derive(Debug, Clone)]
pub struct HotkeyCombo {
    pub modifiers: HashSet<String>,
    pub trigger: HotkeyTrigger,
    pub all_modifier_names: HashSet<String>,
}

#[derive(Debug, Clone)]
pub enum HotkeyTrigger {
    Key(u32),
    Mouse(String),
}

const MOUSE_TRIGGERS: &[&str] = &["mouse1", "mouse2", "mouse3", "mouse4", "mouse5"];

/// Parse a hotkey string like "ctrl+space" or "cmd+f9" into modifiers + trigger.
pub fn parse_hotkey_combo(hotkey_str: &str) -> HotkeyCombo {
    let parts: Vec<&str> = hotkey_str.split('+').map(|s| s.trim()).collect();
    let modifier_names: HashSet<&str> = ["ctrl", "alt", "shift", "win", "cmd"].into_iter().collect();

    let mut modifiers = HashSet::new();
    let last = parts.last().map(|s| s.to_lowercase()).unwrap_or_default();

    for part in &parts[..parts.len().saturating_sub(1)] {
        let lower = part.to_lowercase();
        if modifier_names.contains(lower.as_str()) {
            // "cmd" is an alias for "win" (maps to Command on macOS, Win on Windows).
            let normalized = if lower == "cmd" { "win".to_string() } else { lower };
            modifiers.insert(normalized);
        }
    }

    // Check if the trigger is a mouse button.
    if MOUSE_TRIGGERS.contains(&last.as_str()) {
        return HotkeyCombo {
            all_modifier_names: modifiers.clone(),
            modifiers,
            trigger: HotkeyTrigger::Mouse(last),
        };
    }

    // Check for vk<N> syntax
    if last.starts_with("vk") {
        if let Ok(vk) = last[2..].parse::<u32>() {
            return HotkeyCombo {
                all_modifier_names: modifiers.clone(),
                modifiers,
                trigger: HotkeyTrigger::Key(vk),
            };
        }
    }

    // Try named keys (including modifier names as standalone triggers).
    // All key codes use Windows VK values as the canonical representation;
    // the macOS event tap callback translates native keycodes to VK.
    let trigger_lower = if last == "cmd" { "win".to_string() } else { last.clone() };
    let vk = match trigger_lower.as_str() {
        "ctrl" => 0x11,
        "alt" => 0x12,
        "shift" => 0x10,
        "win" => 0x5B,
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "escape" | "esc" => 0x1B,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "page_up" => 0x21,
        "pagedown" | "page_down" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "f1" => 0x70,
        "f2" => 0x71,
        "f3" => 0x72,
        "f4" => 0x73,
        "f5" => 0x74,
        "f6" => 0x75,
        "f7" => 0x76,
        "f8" => 0x77,
        "f9" => 0x78,
        "f10" => 0x79,
        "f11" => 0x7A,
        "f12" => 0x7B,
        "capslock" | "caps_lock" => 0x14,
        "numlock" | "num_lock" => 0x90,
        "scrolllock" | "scroll_lock" => 0x91,
        "printscreen" | "print_screen" => 0x2C,
        "pause" => 0x13,
        c if c.len() == 1 => {
            let ch = c.chars().next().unwrap();
            char_to_vk(ch).unwrap_or(0x78)
        }
        _ => {
            log::warn!("Could not resolve key '{}' â€” falling back to F9.", last);
            0x78
        }
    };

    let mut all_modifier_names = modifiers.clone();
    let canonical = ["ctrl", "alt", "shift", "win"];
    if canonical.contains(&trigger_lower.as_str()) {
        all_modifier_names.insert(trigger_lower);
    }

    HotkeyCombo {
        modifiers,
        trigger: HotkeyTrigger::Key(vk),
        all_modifier_names,
    }
}

/// Map a single character to its Windows VK code.
#[cfg(target_os = "windows")]
fn char_to_vk(ch: char) -> Option<u32> {
    unsafe {
        let result = windows::Win32::UI::Input::KeyboardAndMouse::VkKeyScanW(ch as u16);
        if result == -1i16 || result as u16 == 0xFFFF {
            None
        } else {
            Some((result as u16 & 0xFF) as u32)
        }
    }
}

/// Map a single character to its Windows VK code (macOS fallback: ASCII mapping).
#[cfg(target_os = "macos")]
fn char_to_vk(ch: char) -> Option<u32> {
    let upper = ch.to_ascii_uppercase();
    if upper.is_ascii_alphabetic() {
        Some(upper as u32) // A=0x41 .. Z=0x5A
    } else if upper.is_ascii_digit() {
        Some(upper as u32) // 0=0x30 .. 9=0x39
    } else {
        None
    }
}

pub(crate) fn vk_to_modifier(vk: u32) -> Option<&'static str> {
    match vk {
        0xA0 | 0xA1 | 0x10 => Some("shift"),
        0xA2 | 0xA3 | 0x11 => Some("ctrl"),
        0xA4 | 0xA5 | 0x12 => Some("alt"),
        0x5B | 0x5C => Some("win"),
        _ => None,
    }
}

// ── Callback actions

pub struct HotkeyCallbacks {
    pub on_hold_press: Box<dyn Fn() + Send>,
    pub on_hold_release: Box<dyn Fn() + Send>,
    pub on_handsfree_press: Box<dyn Fn() + Send>,
    /// Called with the captured combo string when rebind mode is active and
    /// all keys are released. `None` = no rebind UI active.
    pub on_rebind_captured: Option<Box<dyn Fn(String) + Send>>,
}

// ── VK code → combo string helper

/// Map a Windows VK code back to the canonical combo-part name.
fn vk_to_combo_part(vk: u32) -> String {
    match vk {
        0xA2 | 0xA3 | 0x11 => "ctrl".into(),
        0xA4 | 0xA5 | 0x12 => "alt".into(),
        0xA0 | 0xA1 | 0x10 => "shift".into(),
        0x5B | 0x5C => "win".into(),
        0x20 => "space".into(),
        0x0D | 0x6C => "enter".into(),
        0x09 => "tab".into(),
        0x1B => "esc".into(),
        0x08 => "backspace".into(),
        0x2E => "delete".into(),
        0x2D => "insert".into(),
        0x24 => "home".into(),
        0x23 => "end".into(),
        0x21 => "page_up".into(),
        0x22 => "page_down".into(),
        0x26 => "up".into(),
        0x28 => "down".into(),
        0x25 => "left".into(),
        0x27 => "right".into(),
        0x14 => "caps_lock".into(),
        0x90 => "num_lock".into(),
        0x91 => "scroll_lock".into(),
        0x2C => "print_screen".into(),
        0x13 => "pause".into(),
        0x5D => "contextmenu".into(),
        // Numpad digits
        0x60..=0x69 => format!("numpad{}", vk - 0x60),
        // Numpad operators
        0x6A => "numpadmultiply".into(),
        0x6B => "numpadadd".into(),
        0x6D => "numpadsubtract".into(),
        0x6E => "numpaddecimal".into(),
        0x6F => "numpaddivide".into(),
        // F1–F24
        0x70..=0x87 => format!("f{}", vk - 0x70 + 1),
        // Letters A–Z
        0x41..=0x5A => ((vk as u8) as char).to_ascii_lowercase().to_string(),
        // Digits 0–9
        0x30..=0x39 => char::from_digit(vk - 0x30, 10).unwrap_or('?').to_string(),
        // OEM / punctuation
        0xBA => "semicolon".into(),
        0xBB => "equal".into(),
        0xBC => "comma".into(),
        0xBD => "minus".into(),
        0xBE => "period".into(),
        0xBF => "slash".into(),
        0xC0 => "backquote".into(),
        0xDB => "bracketleft".into(),
        0xDC => "backslash".into(),
        0xDD => "bracketright".into(),
        0xDE => "quote".into(),
        // Media keys
        0xB3 => "mediaplaypause".into(),
        0xB2 => "mediastop".into(),
        0xB0 => "medianexttrack".into(),
        0xB1 => "mediaprevioustrack".into(),
        0xAF => "volumeup".into(),
        0xAE => "volumedown".into(),
        0xAD => "volumemute".into(),
        _ => format!("vk{vk}"),
    }
}

// ── Worker thread — platform-agnostic hotkey matching

/// Minimum time (ms) between hold-release and the next hold-press.
/// Prevents rapid toggle on keyboards that send key-up/key-down pairs
/// during auto-repeat (common on gaming keyboards like Razer).
const HOLD_DEBOUNCE_MS: u128 = 200;

pub(super) fn hotkey_worker(
    receiver: Receiver<HotkeyEvent>,
    stop_flag: Arc<AtomicBool>,
    hold_combo: HotkeyCombo,
    handsfree_combo: HotkeyCombo,
    callbacks: HotkeyCallbacks,
) {
    let mut active_modifiers: HashSet<String> = HashSet::new();
    let mut hold_active = false;
    let mut pressed_keys: HashSet<u32> = HashSet::new();
    let mut last_hold_release: Option<std::time::Instant> = None;

    // Rebind capture state
    let mut rebind_pressed: HashSet<u32> = HashSet::new();
    let mut rebind_modifiers: HashSet<u32> = HashSet::new();
    let mut rebind_trigger: Option<u32> = None;

    log::info!(
        "Hotkey worker running -- hold: {:?}, hands-free: {:?}",
        hold_combo, handsfree_combo
    );

    while !stop_flag.load(Ordering::Relaxed) {
        let event = match receiver.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(e) => e,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        match event {
            HotkeyEvent::KeyPress { vk, injected } => {
                if injected {
                    continue;
                }

                // Skip auto-repeat: if the key is already tracked as held,
                // this is a repeat event from the OS — ignore it.
                if !pressed_keys.insert(vk) {
                    continue;
                }

                // Rebind mode: collect keys for combo capture; skip normal matching.
                if REBIND_MODE_ACTIVE.load(Ordering::Relaxed) {
                    rebind_pressed.insert(vk);
                    if vk_to_modifier(vk).is_some() {
                        rebind_modifiers.insert(vk);
                    } else {
                        rebind_trigger = Some(vk);
                    }
                    continue;
                }

                if let Some(mod_name) = vk_to_modifier(vk) {
                    active_modifiers.insert(mod_name.to_string());
                }

                let check_combo = |combo: &HotkeyCombo| -> bool {
                    if let HotkeyTrigger::Key(trigger_vk) = &combo.trigger {
                        if !combo.all_modifier_names.is_empty()
                            && vk_to_modifier(*trigger_vk).is_some()
                        {
                            return combo
                                .all_modifier_names
                                .iter()
                                .all(|m| active_modifiers.contains(m));
                        }
                        vk == *trigger_vk
                            && combo.modifiers.iter().all(|m| active_modifiers.contains(m))
                    } else {
                        false
                    }
                };

                if check_combo(&hold_combo) {
                    if !hold_active {
                        // Debounce: don't re-activate within HOLD_DEBOUNCE_MS
                        // of the last release. This prevents rapid toggle on
                        // keyboards that send press-release pairs as repeat.
                        let debounce_ok = last_hold_release
                            .map(|t| t.elapsed().as_millis() > HOLD_DEBOUNCE_MS)
                            .unwrap_or(true);
                        if debounce_ok {
                            hold_active = true;
                            (callbacks.on_hold_press)();
                        }
                    }
                    continue;
                }

                if check_combo(&handsfree_combo) {
                    log::info!("Hands-free hotkey matched");
                    (callbacks.on_handsfree_press)();
                    continue;
                }
            }

            HotkeyEvent::KeyRelease { vk, injected } => {
                if injected {
                    continue;
                }

                pressed_keys.remove(&vk);

                // Rebind mode: finalize when all tracked keys are released.
                if REBIND_MODE_ACTIVE.load(Ordering::Relaxed) {
                    rebind_pressed.remove(&vk);
                    if rebind_pressed.is_empty() {
                        if let Some(trigger_vk) = rebind_trigger.take() {
                            // Build combo: canonical modifier order + trigger.
                            let mut parts: Vec<String> = Vec::new();
                            for (check_vks, name) in &[
                                (&[0xA2u32, 0xA3, 0x11][..], "ctrl"),
                                (&[0xA4u32, 0xA5, 0x12][..], "alt"),
                                (&[0xA0u32, 0xA1, 0x10][..], "shift"),
                                (&[0x5Bu32, 0x5C][..], "win"),
                            ] {
                                if check_vks.iter().any(|v| rebind_modifiers.contains(v)) {
                                    parts.push((*name).to_string());
                                }
                            }
                            parts.push(vk_to_combo_part(trigger_vk));
                            let combo = parts.join("+");
                            log::info!("Rebind captured: {}", combo);
                            if let Some(ref cb) = callbacks.on_rebind_captured {
                                cb(combo);
                            }
                        }
                        rebind_modifiers.clear();
                        rebind_trigger = None;
                        hold_active = false;
                    }
                    if let Some(mod_name) = vk_to_modifier(vk) {
                        active_modifiers.remove(mod_name);
                    }
                    continue;
                }

                if hold_active {
                    let mut should_release = false;
                    if let HotkeyTrigger::Key(trigger_vk) = &hold_combo.trigger {
                        if vk == *trigger_vk {
                            should_release = true;
                        }
                        if vk_to_modifier(*trigger_vk).is_some() {
                            if let Some(mod_name) = vk_to_modifier(vk) {
                                if hold_combo.all_modifier_names.contains(mod_name) {
                                    should_release = true;
                                }
                            }
                        }
                    }
                    if should_release {
                        hold_active = false;
                        last_hold_release = Some(std::time::Instant::now());
                        log::info!("Hold hotkey released");
                        (callbacks.on_hold_release)();
                    }
                }

                if let Some(mod_name) = vk_to_modifier(vk) {
                    active_modifiers.remove(mod_name);
                }
            }

            HotkeyEvent::MousePress { button } => {
                let btn_name = button.name().to_string();

                if let HotkeyTrigger::Mouse(ref trigger_name) = hold_combo.trigger {
                    if btn_name == *trigger_name
                        && hold_combo
                            .modifiers
                            .iter()
                            .all(|m| active_modifiers.contains(m))
                    {
                        if !hold_active {
                            hold_active = true;
                            (callbacks.on_hold_press)();
                        }
                        continue;
                    }
                }

                if let HotkeyTrigger::Mouse(ref trigger_name) = handsfree_combo.trigger {
                    if btn_name == *trigger_name
                        && handsfree_combo
                            .modifiers
                            .iter()
                            .all(|m| active_modifiers.contains(m))
                    {
                        (callbacks.on_handsfree_press)();
                    }
                }
            }

            HotkeyEvent::MouseRelease { button } => {
                if hold_active {
                    let btn_name = button.name().to_string();
                    if let HotkeyTrigger::Mouse(ref trigger_name) = hold_combo.trigger {
                        if btn_name == *trigger_name {
                            hold_active = false;
                            (callbacks.on_hold_release)();
                        }
                    }
                }
            }
        }
    }
}
