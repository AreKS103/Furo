//! Project Furo — Tauri v2 Application Core
//!
//! Sets up:
//!   1. System tray with Open / Quit menu
//!   2. Hidden frameless "widget" window for the floating dictation pill
#![allow(dead_code)]
//!   3. Native Rust backend pipeline (audio, VAD, Whisper, hotkeys, typer)
//!   4. Close-to-tray behaviour for the main window
//!   5. Persistent store plugin for dictation history

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

// ============================================================================
// Tauri commands (replace REST API endpoints)
// ============================================================================

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

// ============================================================================
// Tray menu
// ============================================================================

fn build_tray_menu(app: &tauri::App) -> Result<tauri::menu::Menu<tauri::Wry>, Box<dyn std::error::Error>> {
    let open_item = MenuItemBuilder::new("Open Furo").id("open").build(app)?;
    let update_item = MenuItemBuilder::new("Check for Update").id("check_update").build(app)?;
    let quit_item = MenuItemBuilder::new("Quit").id("quit").build(app)?;
    Ok(MenuBuilder::new(app).item(&open_item).item(&update_item).separator().item(&quit_item).build()?)
}
// ============================================================================
// Application entry
// ============================================================================

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
            // Managed state

            // ── Settings ─────────────────────────────────────────
            let settings = SettingsStore::new(None);

            // ── Pipeline ─────────────────────────────────────────
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
            let y = screen_size.height as f64 / scale - widget_h - 60.0;

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

            // .transparent() / .shadow() are not available on macOS WebviewWindowBuilder
            #[cfg(target_os = "windows")]
            let builder = builder.transparent(true).shadow(false);

            let _widget = builder.build()?;

            // ── System tray ─────────────────────────────────────
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

            // ── Listen for tray-update events from the frontend ─
            // (reserved for future use)

            // ── Start backend pipeline ──────────────────────────
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
