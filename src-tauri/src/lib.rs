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

#[tauri::command]
fn widget_hold_start(pipeline: tauri::State<'_, Arc<FuroPipeline>>) {
    pipeline.on_hold_press();
}

#[tauri::command]
fn widget_hold_release(pipeline: tauri::State<'_, Arc<FuroPipeline>>) {
    pipeline.on_hold_release();
}

/// Resize the widget window to match the visual pill size.
/// Collapsed = 40×10 (pill at scale-50), Expanded = 80×20 (pill at scale-100).
/// Center stays pinned so the pill morphs in place.
#[tauri::command]
fn widget_set_expanded(app: tauri::AppHandle, expanded: bool) {
    let Some(win) = app.get_webview_window("widget") else { return };
    let Ok(scale) = win.scale_factor() else { return };
    let Ok(pos) = win.outer_position() else { return };
    let Ok(size) = win.outer_size() else { return };

    let (new_w, new_h): (f64, f64) = if expanded { (80.0, 20.0) } else { (40.0, 10.0) };

    // Convert physical→logical for anchor math
    let cur_w = size.width as f64 / scale;
    let cur_h = size.height as f64 / scale;
    let cur_x = pos.x as f64 / scale;
    let cur_y = pos.y as f64 / scale;

    // Keep center pinned so the pill morphs in place
    let center_x = cur_x + cur_w / 2.0;
    let center_y = cur_y + cur_h / 2.0;
    let new_x = center_x - new_w / 2.0;
    let new_y = center_y - new_h / 2.0;

    use tauri::{LogicalPosition, LogicalSize};
    let _ = win.set_size(LogicalSize::new(new_w, new_h));
    let _ = win.set_position(LogicalPosition::new(new_x, new_y));
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
            get_autostart,
            set_autostart,
            preview_sound,
            scan_whisper_models,
            set_rebind_mode,
            widget_hold_start,
            widget_hold_release,
            widget_set_expanded,
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
            .focused(false);

            // Transparent window so the pill composites over the desktop.
            // Without transparent(true), the webview renders an opaque background
            // even when CSS says `background: transparent`.
            let builder = builder.transparent(true);
            #[cfg(target_os = "windows")]
            let builder = builder.shadow(false);

            let _widget = builder.build()?;

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
