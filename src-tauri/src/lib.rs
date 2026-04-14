#![allow(dead_code)]
//! Tauri application entry point — wires audio, VAD, Whisper, hotkeys, and typer into the Tauri lifecycle.

mod audio;
mod config;
mod dsp;
mod hotkey;
mod pipeline;
mod processor;
mod settings;
mod sidecar;
mod transcriber;
mod typer;
mod vad;

use std::collections::HashMap;
use std::sync::Arc;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

use crate::audio::MicInfo;
use crate::pipeline::FuroPipeline;
use crate::settings::SettingsStore;
use crate::transcriber::{scan_for_whisper_models, FoundModel};
use crate::hotkey::REBIND_MODE_ACTIVE;

use tauri_plugin_autostart::MacosLauncher;

// ── macOS: CoreGraphics cursor position for widget hover tracking ─────────
// WKWebView does not receive mousemove events when another application is the
// key window. DOM onMouseEnter/Leave are therefore dead unless Furo is
// frontmost. The workaround is a Rust polling thread that checks the cursor
// position via CoreGraphics every 50 ms and emits Tauri `widget-hover` events
// to the widget — the same approach used by system HUDs like Wispr Flow.
#[cfg(target_os = "macos")]
mod cg_cursor {
    use std::ffi::c_void;

    /// A 2-D point in CoreGraphics global display coordinates.
    /// Origin is at the top-left corner of the primary screen;
    /// coordinates are in logical points (same unit iOS/macOS use in APIs).
    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        /// Create a CoreGraphics event. Called with a NULL source returns an
        /// event whose `location` field reflects the current cursor position.
        pub fn CGEventCreate(source: *const c_void) -> *mut c_void;
        /// Return the cursor location encoded in a CG event.
        pub fn CGEventGetLocation(event: *mut c_void) -> CGPoint;
        /// Release a CoreFoundation object.
        pub fn CFRelease(cf: *const c_void);
    }
}

/// macOS only: spawn a background thread that polls the cursor position every
/// 50 ms and emits `widget-hover` (bool) into the widget WebView whenever the
/// hover state changes.
///
/// Uses two zones for hysteresis:
///   • Activation zone: tight around the COLLAPSED pill (visually 40×10 logical
///     px via CSS transform, at the bottom-center of the always-80×20 window).
///     Cursor must reach the pill area to start hover.
///   • Exit zone: current window bounds + small padding. Once hovering, the
///     cursor can roam over the full expanded pill / popup without dropping out.
///
/// Note: the window is always 80×20 (or 80×62 with popup). Only popup open/close
/// triggers a window resize — hover expand/collapse is pure CSS transform.
#[cfg(target_os = "macos")]
fn start_widget_hover_tracker(widget: tauri::WebviewWindow) {
    const POLL_MS: u64 = 50;
    // X activation — matches the collapsed visual pill width (40px = scaleX(0.5) of 80px).
    const PILL_HALF_W: f64 = 20.0;
    // Y activation — anchored to window bottom (transform-origin: bottom center keeps
    // the collapsed pill bottom flush with the window bottom).
    // Collapsed pill height = 10px (scaleY(0.5) of 20px).
    const PILL_H: f64 = 10.0;
    // Extra headroom above pill: user approaches from above.
    // With always-80×20 window, add a bit more so activation triggers before
    // the cursor reaches the lower half of the window.
    const Y_PAD_TOP: f64 = 8.0;
    const Y_PAD_BOT: f64 = 1.0;
    // Exit zone — exact window bounds + 2px.
    const EXIT_PAD: f64 = 2.0;

    std::thread::Builder::new()
        .name("furo-widget-hover".into())
        .spawn(move || {
            let mut was_hovering = false;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(POLL_MS));

                // Query the current cursor position in CoreGraphics global
                // display coords (logical points, top-left origin, Y down).
                let cursor = unsafe {
                    use cg_cursor::*;
                    let ev = CGEventCreate(std::ptr::null());
                    if ev.is_null() {
                        continue;
                    }
                    let pt = CGEventGetLocation(ev);
                    CFRelease(ev);
                    pt
                };

                // Fetch widget frame. Returns Err when the window is closed.
                let (pos, size, scale) = match (
                    widget.outer_position(),
                    widget.outer_size(),
                    widget.scale_factor(),
                ) {
                    (Ok(p), Ok(s), Ok(sc)) => (p, s, sc),
                    _ => break, // window gone — exit thread
                };

                // Convert physical window frame to logical points.
                let lx = pos.x as f64 / scale;
                let ly = pos.y as f64 / scale;
                let lw = size.width as f64 / scale;
                let lh = size.height as f64 / scale;

                // Activation: exact collapsed pill width in X (no padding),
                // and a small asymmetric band in Y anchored to the window
                // bottom (where the pill always sits). Extra headroom above
                // the pill is necessary because 50ms polling can miss a 10px
                // target if the cursor moves fast — but the user always
                // approaches from above, so only upward tolerance is added.
                let center_x = lx + lw / 2.0;
                let bottom = ly + lh; // stable: widget_set_size pins bottom edge
                let in_activation =
                    (cursor.x - center_x).abs() <= PILL_HALF_W
                        && cursor.y >= bottom - PILL_H - Y_PAD_TOP
                        && cursor.y <= bottom + Y_PAD_BOT;

                // Exit: current window bounds + 2px jitter margin.
                // When expanded (80×20 or 80×62) this naturally covers the
                // full expanded pill and popup area.
                let in_exit =
                    cursor.x >= lx - EXIT_PAD
                        && cursor.x <= lx + lw + EXIT_PAD
                        && cursor.y >= ly - EXIT_PAD
                        && cursor.y <= ly + lh + EXIT_PAD;

                let is_hovering = if was_hovering { in_exit } else { in_activation };

                if is_hovering != was_hovering {
                    was_hovering = is_hovering;
                    let _ = widget.emit("widget-hover", is_hovering);
                }
            }
            log::info!("Widget hover tracker exited.");
        })
        .ok();
}

// ── Fullscreen detection ──────────────────────────────────────────────────
// Polls every 500 ms to detect whether any external application is using the
// full screen (browser fullscreen, VLC, Windows Video Player, YouTube, etc.).
// Emits `widget-fullscreen` (bool) so the widget can fade out gracefully.
//
// macOS implementation uses CGWindowListCopyWindowInfo to check if any
// non-Furo, normal-level window fills the main display's logical resolution.
//
// Windows implementation uses GetForegroundWindow + GetMonitorInfo to check
// if the foreground window exactly covers its monitor.

/// macOS: FFI for window-list-based fullscreen detection.
#[cfg(target_os = "macos")]
mod cg_fullscreen {
    use std::ffi::c_void;

    /// A CoreGraphics rectangle (logical points).
    #[repr(C)]
    #[derive(Clone, Copy, Default, Debug)]
    pub struct CGRect {
        pub x: f64,
        pub y: f64,
        pub width: f64,
        pub height: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        /// Returns CFArrayRef of window dicts (on-screen, excluding desktop elements).
        pub fn CGWindowListCopyWindowInfo(opts: u32, relative_to: u32) -> *mut c_void;
        /// Returns the display ID of the primary display.
        pub fn CGMainDisplayID() -> u32;
        /// Returns the logical bounding rect of `display` in global display coords.
        /// Available since macOS 10.3 — simpler than querying CGDisplayMode.
        pub fn CGDisplayBounds(display: u32) -> CGRect;
        /// Parses a `kCGWindowBounds` CFDictionary into a CGRect (logical points).
        pub fn CGRectMakeWithDictionaryRepresentation(
            dict: *const c_void,
            rect: *mut CGRect,
        ) -> bool;
        pub fn CFRelease(cf: *const c_void);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFArrayGetCount(arr: *const c_void) -> isize;
        pub fn CFArrayGetValueAtIndex(arr: *const c_void, idx: isize) -> *const c_void;
    }
}

/// macOS: returns true if any non-Furo window currently fills the main display.
#[cfg(target_os = "macos")]
fn check_fullscreen_active(own_pid: i32) -> bool {
    use std::ffi::{c_char, c_void};
    use cg_fullscreen::*;
    use objc::{class, msg_send, sel, sel_impl};

    // kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements
    const OPTS: u32 = 1 | 16;

    unsafe {
        // Get logical bounds of the primary display via CGDisplayBounds.
        // Available since macOS 10.3; no separate mode query needed.
        let display_id = CGMainDisplayID();
        let screen_rect = CGDisplayBounds(display_id);
        let screen_w = screen_rect.width;
        let screen_h = screen_rect.height;

        if screen_w < 1.0 || screen_h < 1.0 {
            return false;
        }

        let arr = CGWindowListCopyWindowInfo(OPTS, 0 /* kCGNullWindowID */);
        if arr.is_null() {
            return false;
        }

        let count = CFArrayGetCount(arr);
        let mut found_fullscreen = false;

        // NSString key creation and NSDictionary lookups use ObjC → need a pool.
        let pool: *mut objc::runtime::Object =
            msg_send![class!(NSAutoreleasePool), new];

        'outer: for i in 0..count {
            let dict = CFArrayGetValueAtIndex(arr, i) as *mut objc::runtime::Object;
            if dict.is_null() {
                continue;
            }

            // ── Skip our own process's windows ──────────────────
            let key_pid: *mut objc::runtime::Object = {
                let s = b"kCGWindowOwnerPID\0";
                msg_send![class!(NSString), stringWithUTF8String: s.as_ptr() as *const c_char]
            };
            let pid_obj: *mut objc::runtime::Object =
                msg_send![dict, objectForKey: key_pid];
            if !pid_obj.is_null() {
                let pid: i32 = msg_send![pid_obj, intValue];
                if pid == own_pid {
                    continue;
                }
            }

            // ── Only normal-level windows (layer 0) ─────────────
            // Fullscreen apps and standard windows are both at layer 0.
            let key_layer: *mut objc::runtime::Object = {
                let s = b"kCGWindowLayer\0";
                msg_send![class!(NSString), stringWithUTF8String: s.as_ptr() as *const c_char]
            };
            let layer_obj: *mut objc::runtime::Object =
                msg_send![dict, objectForKey: key_layer];
            if !layer_obj.is_null() {
                let layer: i32 = msg_send![layer_obj, intValue];
                if layer != 0 {
                    continue;
                }
            }

            // ── Parse window bounds ──────────────────────────────
            let key_bounds: *mut objc::runtime::Object = {
                let s = b"kCGWindowBounds\0";
                msg_send![class!(NSString), stringWithUTF8String: s.as_ptr() as *const c_char]
            };
            let bounds_obj: *mut objc::runtime::Object =
                msg_send![dict, objectForKey: key_bounds];
            if bounds_obj.is_null() {
                continue;
            }

            let mut rect = CGRect::default();
            let ok: bool = CGRectMakeWithDictionaryRepresentation(
                bounds_obj as *const c_void,
                &mut rect,
            );
            if !ok {
                continue;
            }

            // ── Fullscreen if window fills the display (±2pt tolerance) ──
            if rect.width >= screen_w - 2.0 && rect.height >= screen_h - 2.0 {
                found_fullscreen = true;
                break 'outer;
            }
        }

        let () = msg_send![pool, drain];
        CFRelease(arr);
        found_fullscreen
    }
}

/// Windows: returns true if the foreground window fills its entire monitor.
#[cfg(target_os = "windows")]
fn check_fullscreen_active() -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }

        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            return false;
        }

        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut mi).as_bool() {
            return false;
        }

        let mr = mi.rcMonitor;
        // Fullscreen: window rect equals monitor rect exactly.
        // Also require minimum monitor-sized dimensions to filter out tiny windows.
        let w = wr.right - wr.left;
        let h = wr.bottom - wr.top;
        w >= 640
            && h >= 480
            && wr.left == mr.left
            && wr.top == mr.top
            && wr.right == mr.right
            && wr.bottom == mr.bottom
    }
}

/// Cross-platform: spawn a background thread that polls for fullscreen state
/// every 500 ms and emits `widget-fullscreen` (bool) into the widget WebView.
fn start_fullscreen_tracker(widget: tauri::WebviewWindow) {
    const POLL_MS: u64 = 500;

    std::thread::Builder::new()
        .name("furo-fullscreen".into())
        .spawn(move || {
            #[cfg(target_os = "macos")]
            let own_pid = std::process::id() as i32;

            let mut was_fullscreen = false;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(POLL_MS));

                #[cfg(target_os = "macos")]
                let is_fs = check_fullscreen_active(own_pid);

                #[cfg(target_os = "windows")]
                let is_fs = check_fullscreen_active();

                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                let is_fs = false;

                if is_fs != was_fullscreen {
                    was_fullscreen = is_fs;
                    let _ = widget.emit("widget-fullscreen", is_fs);
                }
            }
        })
        .ok();
}

// ── Tauri commands

#[tauri::command]
fn get_settings(pipeline: tauri::State<'_, Arc<FuroPipeline>>) -> HashMap<String, String> {
    pipeline.settings.all()
}

#[tauri::command]
fn update_settings(
    pipeline: tauri::State<'_, Arc<FuroPipeline>>,
    data: HashMap<String, String>,
) -> HashMap<String, String> {
    let hotkey_changed = data.contains_key("hotkey_hold") || data.contains_key("hotkey_handsfree");

    pipeline.settings.update(data);

    if hotkey_changed {
        pipeline.start_hotkey_listener();
    }

    pipeline.emit_settings();
    pipeline.settings.all()
}

#[tauri::command]
fn list_microphones(pipeline: tauri::State<'_, Arc<FuroPipeline>>) -> Vec<MicInfo> {
    pipeline.list_microphones()
}

/// Diagnostic command — returns pipeline state to help debug hotkey issues.
#[tauri::command]
fn test_pipeline(pipeline: tauri::State<'_, Arc<FuroPipeline>>) -> String {
    let ready = pipeline.is_ready();
    let mode = pipeline.current_mode();
    let hold = pipeline.settings.get("hotkey_hold");
    let hf = pipeline.settings.get("hotkey_handsfree");
    let mic = pipeline.settings.get("microphone");
    let diag = format!(
        "models_ready={}\nmode={:?}\nhotkey_hold={}\nhotkey_handsfree={}\nmicrophone={}",
        ready, mode, hold, hf, mic
    );
    log::info!("test_pipeline:\n{}", diag);
    diag
}

#[tauri::command]
fn get_autostart(app: tauri::AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    if enabled {
        mgr.enable().map_err(|e| format!("Failed to enable autostart: {}", e))
    } else {
        mgr.disable().map_err(|e| format!("Failed to disable autostart: {}", e))
    }
}

#[tauri::command]
fn preview_sound(pipeline: tauri::State<'_, Arc<FuroPipeline>>) {
    pipeline.preview_sound();
}

/// Scan the system for existing GGML-format Whisper model files.
/// Returns a list of found models sorted by quality (largest first).
#[tauri::command]
fn scan_whisper_models(pipeline: tauri::State<'_, Arc<FuroPipeline>>) -> Vec<FoundModel> {
    let models_dir = pipeline.models_dir();
    scan_for_whisper_models(&models_dir)
}

/// Activate or deactivate rebind-capture mode.
/// While active, the Win32 hook suppresses Win key OS defaults (Start menu,
/// language switcher) and the hotkey worker captures the next key combo,
/// emitting it as a `furo://rebind-capture` event.
#[tauri::command]
fn set_rebind_mode(active: bool) {
    REBIND_MODE_ACTIVE.store(active, std::sync::atomic::Ordering::SeqCst);
    log::info!("Rebind mode: {}", active);
}

/// Retry starting the hotkey listener after the user grants Accessibility
/// permission. Avoids requiring a full app restart.
#[tauri::command]
fn retry_hotkey_listener(pipeline: tauri::State<'_, Arc<FuroPipeline>>) {
    pipeline.start_hotkey_listener();
}

/// macOS diagnostic command — gathers system state for remote debugging.
/// Returns a multi-line string with sidecar status, permissions, quarantine,
/// model paths, and audio device info.
#[tauri::command]
fn diagnose_macos(pipeline: tauri::State<'_, Arc<FuroPipeline>>) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("OS: {}", std::env::consts::OS));
    lines.push(format!("ARCH: {}", std::env::consts::ARCH));

    // Executable path
    if let Ok(exe) = std::env::current_exe() {
        lines.push(format!("EXE: {}", exe.display()));

        // Detect App Translocation – macOS runs apps from a temp dir when
        // they haven't been moved to /Applications after download.
        #[cfg(target_os = "macos")]
        {
            let path_str = exe.display().to_string();
            if path_str.contains("AppTranslocation") || path_str.starts_with("/private/var/folders/") {
                lines.push("APP_TRANSLOCATION: true ⚠️ Drag Furo to /Applications and relaunch".to_string());
            } else {
                lines.push("APP_TRANSLOCATION: false".to_string());
            }
        }

        // Check Accessibility / Input Monitoring permission.
        #[cfg(target_os = "macos")]
        {
            extern "C" { fn AXIsProcessTrusted() -> bool; }
            let trusted = unsafe { AXIsProcessTrusted() };
            lines.push(format!("ACCESSIBILITY_TRUSTED: {}", trusted));
            if !trusted {
                lines.push("ACCESSIBILITY_NOTE: ⚠️ Grant Accessibility in System Settings > Privacy & Security for hotkeys".to_string());
            }
        }

        if let Some(exe_dir) = exe.parent() {
            // Check sidecar binary exists
            let sidecar_name = if cfg!(target_os = "windows") { "whisper-server.exe" } else { "whisper-server" };
            let sidecar_path = exe_dir.join(sidecar_name);
            let sidecar_exists = sidecar_path.exists();
            lines.push(format!("SIDECAR_PATH: {}", sidecar_path.display()));
            lines.push(format!("SIDECAR_EXISTS: {}", sidecar_exists));

            // Check execute permission and quarantine on macOS
            #[cfg(target_os = "macos")]
            {
                if sidecar_exists {
                    // Check quarantine xattr
                    if let Ok(output) = std::process::Command::new("xattr")
                        .arg("-l")
                        .arg(&sidecar_path)
                        .output()
                    {
                        let xattrs = String::from_utf8_lossy(&output.stdout);
                        let has_quarantine = xattrs.contains("com.apple.quarantine");
                        lines.push(format!("QUARANTINE: {}", has_quarantine));
                        if has_quarantine {
                            lines.push(format!("XATTRS: {}", xattrs.trim()));
                        }
                    }

                    // Check code signature
                    if let Ok(output) = std::process::Command::new("codesign")
                        .args(["-dvv", &sidecar_path.to_string_lossy()])
                        .output()
                    {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        if stderr.contains("not signed") {
                            lines.push("CODESIGN: not signed".to_string());
                        } else if stderr.contains("valid on disk") {
                            lines.push("CODESIGN: signed (valid)".to_string());
                        } else {
                            lines.push(format!("CODESIGN: {}", stderr.lines().next().unwrap_or("unknown")));
                        }
                    }

                    // Check execute permission
                    if let Ok(meta) = std::fs::metadata(&sidecar_path) {
                        use std::os::unix::fs::PermissionsExt;
                        let mode = meta.permissions().mode();
                        lines.push(format!("PERMISSIONS: {:o}", mode));
                        lines.push(format!("EXECUTABLE: {}", mode & 0o111 != 0));
                    }
                }
            }
        }
    }

    // Whisper server health check
    let health_url = format!("{}/health", crate::config::WHISPER_SERVER_URL);
    lines.push(format!("WHISPER_URL: {}", health_url));
    match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(client) => match client.get(&health_url).send() {
            Ok(resp) => {
                lines.push(format!("WHISPER_STATUS: {} {}", resp.status().as_u16(), resp.status()));
                if let Ok(body) = resp.text() {
                    lines.push(format!("WHISPER_BODY: {}", body.chars().take(200).collect::<String>()));
                }
            }
            Err(e) => lines.push(format!("WHISPER_ERROR: {}", e)),
        },
        Err(e) => lines.push(format!("HTTP_CLIENT_ERROR: {}", e)),
    }

    // Pipeline state
    let ready = pipeline.is_ready();
    let mode = pipeline.current_mode();
    lines.push(format!("PIPELINE_READY: {}", ready));
    lines.push(format!("PIPELINE_MODE: {:?}", mode));

    // Model paths
    let models_dir = pipeline.models_dir();
    lines.push(format!("MODELS_DIR: {}", models_dir.display()));
    lines.push(format!("MODELS_DIR_EXISTS: {}", models_dir.exists()));
    let whisper_model = models_dir.join(crate::config::GGML_MODEL_FILENAME);
    lines.push(format!("WHISPER_MODEL: {}", whisper_model.display()));
    lines.push(format!("WHISPER_MODEL_EXISTS: {}", whisper_model.exists()));
    if whisper_model.exists() {
        if let Ok(meta) = whisper_model.metadata() {
            lines.push(format!("WHISPER_MODEL_SIZE_MB: {:.0}", meta.len() as f64 / 1_048_576.0));
        }
    }
    let vad_model = models_dir.join(crate::config::VAD_MODEL_FILENAME);
    lines.push(format!("VAD_MODEL_EXISTS: {}", vad_model.exists()));

    // Sidecar crash state (from last spawn attempt)
    {
        let sidecar = pipeline.sidecar.lock();
        let exited = sidecar.sidecar_exited.load(std::sync::atomic::Ordering::SeqCst);
        lines.push(format!("SIDECAR_EXITED: {}", exited));
        if exited {
            let stderr = sidecar.stderr_capture.lock();
            let last_lines: Vec<&str> = stderr.iter().rev().take(10).map(|s| s.as_str()).collect();
            let last_lines: Vec<&str> = last_lines.into_iter().rev().collect();
            if !last_lines.is_empty() {
                lines.push(format!("SIDECAR_STDERR_LAST_LINES:\n{}", last_lines.join("\n")));
            }
        }
    }

    // Audio devices
    {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        if let Some(dev) = host.default_input_device() {
            let name = dev.description()
                .map(|d| d.name().to_string())
                .unwrap_or_else(|_| "unknown".into());
            lines.push(format!("DEFAULT_MIC: {}", name));
        } else {
            lines.push("DEFAULT_MIC: none".to_string());
        }
    }

    // Settings
    let hold = pipeline.settings.get("hotkey_hold");
    let hf = pipeline.settings.get("hotkey_handsfree");
    let mic = pipeline.settings.get("microphone");
    lines.push(format!("HOTKEY_HOLD: {}", hold));
    lines.push(format!("HOTKEY_HANDSFREE: {}", hf));
    lines.push(format!("MICROPHONE: {}", mic));

    let result = lines.join("\n");
    log::info!("diagnose_macos:\n{}", result);
    result
}

/// Diagnostics for the updater — checks whether the app location is writable,
/// temp dir is on the same volume, and other common macOS update blockers.
#[tauri::command]
fn diagnose_updater() -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("OS: {}", std::env::consts::OS));
    lines.push(format!("ARCH: {}", std::env::consts::ARCH));

    // Executable path
    if let Ok(exe) = std::env::current_exe() {
        lines.push(format!("EXE: {}", exe.display()));

        #[cfg(target_os = "macos")]
        {
            let path_str = exe.display().to_string();

            // Check App Translocation
            if path_str.contains("AppTranslocation") || path_str.starts_with("/private/var/folders/") {
                lines.push("APP_TRANSLOCATION: true ⚠️ Updates CANNOT work from a translocated path".to_string());
            } else {
                lines.push("APP_TRANSLOCATION: false".to_string());
            }

            // Resolve the .app bundle directory
            // EXE is typically: /Applications/Furo.app/Contents/MacOS/Furo
            if let Some(macos_dir) = exe.parent() {
                if let Some(contents_dir) = macos_dir.parent() {
                    if let Some(app_dir) = contents_dir.parent() {
                        let app_path = app_dir.to_path_buf();
                        lines.push(format!("APP_BUNDLE: {}", app_path.display()));

                        // Check if the .app bundle's parent directory is writable
                        if let Some(parent) = app_path.parent() {
                            let parent_writable = {
                                let test_file = parent.join(".furo_update_test");
                                match std::fs::File::create(&test_file) {
                                    Ok(_) => {
                                        let _ = std::fs::remove_file(&test_file);
                                        true
                                    }
                                    Err(_) => false,
                                }
                            };
                            lines.push(format!("APP_PARENT_DIR: {}", parent.display()));
                            lines.push(format!("APP_PARENT_WRITABLE: {}", parent_writable));
                            if !parent_writable {
                                lines.push("⚠️ Cannot write to app directory — updater will fail".to_string());
                            }
                        }

                        // Check if app itself is writable (the .app bundle)
                        if let Ok(meta) = std::fs::metadata(&app_path) {
                            use std::os::unix::fs::PermissionsExt;
                            let mode = meta.permissions().mode();
                            lines.push(format!("APP_BUNDLE_PERMISSIONS: {:o}", mode));
                        }
                    }
                }
            }

            // Check temp dir — the updater downloads to temp then moves to app location.
            // If they're on different filesystems, rename() fails.
            let temp_dir = std::env::temp_dir();
            lines.push(format!("TEMP_DIR: {}", temp_dir.display()));

            // Check if the app and temp are on the same volume
            // (simple heuristic: compare the first path component after /)
            let app_vol = path_str.split('/').nth(1).unwrap_or("");
            let temp_vol = temp_dir.display().to_string();
            let temp_vol_part = temp_vol.split('/').nth(1).unwrap_or("");
            let same_volume = app_vol == temp_vol_part || app_vol == "Applications";
            lines.push(format!("SAME_VOLUME: {} (app={}, temp={})", same_volume, app_vol, temp_vol_part));
        }

        #[cfg(target_os = "windows")]
        {
            // Check if exe dir is writable
            if let Some(exe_dir) = exe.parent() {
                let writable = {
                    let test_file = exe_dir.join(".furo_update_test");
                    match std::fs::File::create(&test_file) {
                        Ok(_) => {
                            let _ = std::fs::remove_file(&test_file);
                            true
                        }
                        Err(_) => false,
                    }
                };
                lines.push(format!("EXE_DIR_WRITABLE: {}", writable));
            }
        }
    }

    let result = lines.join("\n");
    log::info!("diagnose_updater:\n{}", result);
    result
}

#[tauri::command]
fn widget_hold_start(pipeline: tauri::State<'_, Arc<FuroPipeline>>) {
    pipeline.on_hold_press();
}

#[tauri::command]
fn widget_hold_release(pipeline: tauri::State<'_, Arc<FuroPipeline>>) {
    pipeline.on_hold_release();
}

/// Resize the widget window to an arbitrary logical size.
/// The bottom-center stays pinned so the pill never jumps.
/// On Windows uses SetWindowPos(SWP_NOACTIVATE) so the resize never
/// steals keyboard focus from the user's active text field.
#[tauri::command]
fn widget_set_size(app: tauri::AppHandle, width: f64, height: f64) {
    let Some(win) = app.get_webview_window("widget") else { return };
    let Ok(scale) = win.scale_factor() else { return };
    let Ok(pos) = win.outer_position() else { return };
    let Ok(size) = win.outer_size() else { return };

    // Convert physical → logical for anchor math
    let cur_w = size.width as f64 / scale;
    let cur_h = size.height as f64 / scale;
    let cur_x = pos.x as f64 / scale;
    let cur_y = pos.y as f64 / scale;

    // Pin bottom-center: pill stays at the bottom, popup grows upward
    let bottom_y = cur_y + cur_h;
    let center_x = cur_x + cur_w / 2.0;
    let new_x = center_x - width / 2.0;
    let new_y = bottom_y - height;

    // On Windows, use SetWindowPos with SWP_NOACTIVATE so the resize/move does
    // NOT activate the widget window.  Tauri's set_size + set_position both call
    // SetWindowPos without SWP_NOACTIVATE which activates the widget and steals
    // keyboard focus from whatever the user was typing in.
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
        };
        if let Ok(hwnd_ptr) = win.hwnd() {
            let hwnd = HWND(hwnd_ptr.0);
            let x_phys = (new_x * scale).round() as i32;
            let y_phys = (new_y * scale).round() as i32;
            let w_phys = (width * scale).round() as i32;
            let h_phys = (height * scale).round() as i32;
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    x_phys,
                    y_phys,
                    w_phys,
                    h_phys,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
                );
            }
            return;
        }
    }

    use tauri::{LogicalPosition, LogicalSize};
    let _ = win.set_size(LogicalSize::new(width, height));
    let _ = win.set_position(LogicalPosition::new(new_x, new_y));
}

/// Re-paste the last transcription text (from box click).
/// Writes `text` to the clipboard and sends Cmd+V (macOS) / Ctrl+V (Windows)
/// to the last known external application.
#[tauri::command]
fn repaste_last(text: String, pipeline: tauri::State<'_, Arc<FuroPipeline>>) {
    pipeline.repaste(text);
}

/// Reposition the widget window by physical pixel coordinates without activating it.
/// Called from the multi-monitor repositioning logic in FloatingWidget.tsx.
/// On Windows uses SetWindowPos(SWP_NOACTIVATE) so the move never steals focus.
#[tauri::command]
fn widget_reposition(app: tauri::AppHandle, x: i32, y: i32) {
    let Some(win) = app.get_webview_window("widget") else { return };

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
        };
        if let Ok(hwnd_ptr) = win.hwnd() {
            let hwnd = HWND(hwnd_ptr.0);
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE | SWP_NOOWNERZORDER,
                );
            }
            return;
        }
    }

    use tauri::PhysicalPosition;
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

// ── Tray menu

fn build_tray_menu(app: &tauri::App) -> Result<tauri::menu::Menu<tauri::Wry>, Box<dyn std::error::Error>> {
    let open_item = MenuItemBuilder::new("Open Furo").id("open").build(app)?;
    let update_item = MenuItemBuilder::new("Check for Update").id("check_update").build(app)?;
    let quit_item = MenuItemBuilder::new("Quit").id("quit").build(app)?;
    Ok(MenuBuilder::new(app).item(&open_item).item(&update_item).separator().item(&quit_item).build()?)
}
// ── Application entry

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            update_settings,
            list_microphones,
            test_pipeline,
            diagnose_macos,
            diagnose_updater,
            get_autostart,
            set_autostart,
            preview_sound,
            scan_whisper_models,
            set_rebind_mode,
            retry_hotkey_listener,
            widget_hold_start,
            widget_hold_release,
            widget_reposition,
            repaste_last,
        ])
        .setup(|app| {
            let settings = SettingsStore::new(None);
            let pipeline = FuroPipeline::new(app.handle().clone(), settings);
            app.manage(Arc::clone(&pipeline));

            // ── Widget window (floating dictation pill) ──────────
            let monitor = app
                .primary_monitor()
                .ok()
                .flatten()
                .expect("no primary monitor found");
            let screen_size = monitor.size();
            let scale = monitor.scale_factor();
            // Start at the minimum collapsed footprint (40x10) so it doesn't block the screen behind it on launch.
            // The React `useEffect` dynamically resizes it up to 80x64 as needed.
            let widget_w: f64 = 40.0;
            let widget_h: f64 = 10.0;
            let x = (screen_size.width as f64 / scale - widget_w) / 2.0;
            // macOS Dock occupies ~70-90px at bottom; use a larger offset so
            // the pill isn't hidden behind it.
            let bottom_offset: f64 = if cfg!(target_os = "macos") { 100.0 } else { 60.0 };
            let y = screen_size.height as f64 / scale - widget_h - bottom_offset;

            let builder = WebviewWindowBuilder::new(
                app,
                "widget",
                WebviewUrl::App("index.html?window=widget".into()),
            )
            .title("Furo Widget")
            .inner_size(widget_w, widget_h)
            .position(x, y)
            .decorations(false)
            .always_on_top(true)
            .visible(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .accept_first_mouse(true);

            // Transparent window so the pill composites over the desktop.
            // Without transparent(true), the webview renders an opaque background
            // even when CSS says `background: transparent`.
            let builder = builder.transparent(true);
            #[cfg(target_os = "windows")]
            let builder = builder.shadow(false);

            let _widget = builder.build()?;

            // ── Windows: make the widget non-activating ──────────────
            // Without this, ShowWindow(SW_SHOW) or clicking the widget
            // steals foreground focus from the user's active text field.
            // WS_EX_NOACTIVATE is the Win32 equivalent of macOS's
            // NSWindowStyleMaskNonactivatingPanel.
            #[cfg(target_os = "windows")]
            {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{
                    GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
                };

                if let Ok(hwnd_ptr) = _widget.hwnd() {
                    let hwnd = HWND(hwnd_ptr.0);
                    unsafe {
                        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                        SetWindowLongPtrW(
                            hwnd,
                            GWL_EXSTYLE,
                            ex_style | WS_EX_NOACTIVATE.0 as isize,
                        );
                    }
                    log::info!("Widget HWND configured with WS_EX_NOACTIVATE.");
                }
            }

            // ── macOS: make the widget a non-activating floating panel ──
            // Without this, clicking the widget while another app is focused
            // first activates the Tauri app (stealing focus), requiring a
            // second click to actually trigger mouseDown in the WebView.
            // By setting NSWindowStyleMaskNonactivatingPanel and preventing
            // the window from becoming key, clicks go straight through to
            // the WebView without disturbing the user's active app.
            #[cfg(target_os = "macos")]
            {
                use objc::runtime::{Object, NO};
                use objc::{class, msg_send, sel, sel_impl};

                if let Ok(ns_window_ptr) = _widget.ns_window() {
                    let ns_window = ns_window_ptr as *mut Object;
                    unsafe {
                        // NSFloatingWindowLevel (CGWindowLevelForKey(3)) = 3
                        // keeps widget above normal windows but below alerts.
                        let _: () = msg_send![ns_window, setLevel: 3i64];

                        // Don't hide when the app is deactivated — the widget
                        // must remain visible while the user works in another app.
                        let _: () = msg_send![ns_window, setHidesOnDeactivate: NO];

                        // NSWindowCollectionBehavior:
                        //   canJoinAllSpaces (1 << 0) = 1
                        //   stationary      (1 << 4) = 16
                        //   fullScreenAuxiliary (1 << 8) = 256
                        // This keeps the widget on all desktops/spaces.
                        let behavior: u64 = 1 | 16 | 256;
                        let _: () = msg_send![ns_window, setCollectionBehavior: behavior];

                        // Make the window truly invisible: clear background + no shadow.
                        // Without this the NSWindow draws a faint rect around the webview.
                        let clear: *mut Object = msg_send![class!(NSColor), clearColor];
                        let _: () = msg_send![ns_window, setBackgroundColor: clear];
                        let _: () = msg_send![ns_window, setHasShadow: NO];
                        let _: () = msg_send![ns_window, setOpaque: NO];

                        // NSWindowStyleMask: add nonactivatingPanel bit (1 << 7 = 128)
                        // so clicking the widget does NOT activate the Tauri app.
                        let current_mask: u64 = msg_send![ns_window, styleMask];
                        let _: () = msg_send![ns_window, setStyleMask: current_mask | 128u64];

                        // If this is actually an NSPanel (some Tauri versions
                        // create one for borderless windows), prevent it from
                        // becoming the key window on click.
                        let responds: bool = msg_send![
                            ns_window,
                            respondsToSelector: sel!(setBecomesKeyOnlyIfNeeded:)
                        ];
                        if responds {
                            let _: () = msg_send![
                                ns_window,
                                setBecomesKeyOnlyIfNeeded: objc::runtime::YES
                            ];
                            log::info!("Widget is NSPanel — setBecomesKeyOnlyIfNeeded:YES applied.");
                        }
                    }
                    log::info!("Widget NSWindow configured as non-activating floating panel.");
                }
            }

            // macOS: DOM hover events (onMouseEnter/Leave) are suppressed by
            // the OS whenever another application is the key window — the
            // WKWebView simply never receives mousemove. Start a native
            // polling thread that emits `widget-hover` Tauri events instead.
            #[cfg(target_os = "macos")]
            start_widget_hover_tracker(_widget.clone());

            // All platforms: poll for fullscreen state and fade the widget
            // when the user watches a video (YouTube, VLC, system viewer, etc.).
            start_fullscreen_tracker(_widget.clone());

            let tray_menu = build_tray_menu(app)?;

            let mut tray_builder = TrayIconBuilder::with_id("main-tray")
                .tooltip("Furo")
                .menu(&tray_menu);
            if let Some(icon) = app.default_window_icon().cloned() {
                tray_builder = tray_builder.icon(icon);
            }
            let _tray = tray_builder
                .on_menu_event(move |app_handle: &tauri::AppHandle<tauri::Wry>, event: tauri::menu::MenuEvent| {
                    match event.id().as_ref() {
                        "open" => {
                            if let Some(w) = app_handle.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "check_update" => {
                            if let Some(w) = app_handle.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                            let _ = app_handle.emit("furo://check-update", ());
                        }
                        "quit" => {
                            app_handle.exit(0);
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            // ── Start backend pipeline ──────────────────────────
            // macOS: Warn the user if running under App Translocation
            // (Gatekeeper copies the .app to a read-only temp path).
            #[cfg(target_os = "macos")]
            {
                if let Ok(exe) = std::env::current_exe() {
                    let path = exe.display().to_string();
                    if path.contains("AppTranslocation")
                        || path.starts_with("/private/var/folders/")
                    {
                        log::warn!(
                            "App Translocation detected — Furo is running from a temporary path"
                        );
                        let _ = app.emit("furo://app-translocation", true);
                    }
                }
            }

            // Begin tracking the last external focused window so `capture_target`
            // can remember it even before the user presses the hotkey.
            typer::start_focus_tracker();
            pipeline.start_hotkey_listener();
            let pipeline_bg = Arc::clone(&pipeline);
            std::thread::Builder::new()
                .name("furo-init".into())
                .spawn(move || {
                    pipeline_bg.load_models();
                })
                .expect("Failed to spawn model loading thread");

            Ok(())
        })
        // ── Close-to-tray: hide the main window instead of quitting ─
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Furo")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Gracefully kill sidecar servers on app exit
                let pipeline = app_handle.state::<Arc<FuroPipeline>>();
                pipeline.shutdown();
            }
        });
}
