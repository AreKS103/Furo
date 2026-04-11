//! Project Furo — Sidecar Process Manager
//!
//! Spawns and manages the pre-compiled whisper.cpp HTTP server
//! as a Tauri sidecar process. Handles:
//!   - Starting the server with correct CLI args
//!   - Polling health endpoint until ready
//!   - Graceful shutdown on app exit

use std::path::Path;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

use crate::config;

/// Running sidecar server handle.
pub struct SidecarManager {
    whisper_child: Option<CommandChild>,
}

impl SidecarManager {
    pub fn new() -> Self {
        Self {
            whisper_child: None,
        }
    }

    /// Spawn the whisper.cpp server sidecar.
    ///
    /// Args: `--model <path> --host 127.0.0.1 --port 8080 --flash-attn`
    pub fn start_whisper(
        &mut self,
        app: &AppHandle,
        model_path: &Path,
    ) -> Result<(), String> {
        let model_str = model_path
            .to_str()
            .ok_or("Invalid whisper model path encoding")?;

        // Tauri's NSIS bundler strips the "binaries/" path prefix from externalBin
        // entries when installing, so in production the binary lives at <exe_dir>/whisper-server.exe.
        // In dev mode, build.rs copies it to <exe_dir>/binaries/whisper-server.exe and both
        // root AND binaries/ copies exist. We detect which layout we're in and pick the right
        // sidecar name so tauri-plugin-shell resolves the correct path.
        let exe_dir = std::env::current_exe()
            .map_err(|e| format!("Failed to locate current executable: {}", e))?;
        let exe_dir = exe_dir.parent()
            .ok_or_else(|| "Executable has no parent directory".to_string())?;
        let sidecar_name: &str =
            if exe_dir.join("whisper-server.exe").exists()
                && !exe_dir.join("binaries").join("whisper-server.exe").exists()
            {
                // Production (NSIS): binary at root level, no binaries/ subdir
                "whisper-server"
            } else {
                // Dev (build.rs): binary in binaries/ subdir
                "binaries/whisper-server"
            };
        log::info!("whisper-server sidecar name resolved to: {}", sidecar_name);

        let cmd = app
            .shell()
            .sidecar(sidecar_name)
            .map_err(|e| format!("Failed to create whisper-server sidecar command: {}", e))?
            .args([
                "--model",
                model_str,
                "--host",
                "127.0.0.1",
                "--port",
                &config::WHISPER_SERVER_PORT.to_string(),
                "--flash-attn",
                "--inference-path",
                "/v1/audio/transcriptions",
            ]);

        let (_rx, child) = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn whisper-server: {}", e))?;

        self.whisper_child = Some(child);
        log::info!(
            "whisper-server spawned on port {}",
            config::WHISPER_SERVER_PORT
        );
        Ok(())
    }

    /// Block until a sidecar HTTP server responds to `/health` (or GET `/`).
    /// Returns Ok(()) when ready, Err on timeout.
    pub fn wait_for_ready(base_url: &str, name: &str) -> Result<(), String> {
        let health_url = format!("{}/health", base_url);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| format!("HTTP client build error: {}", e))?;

        let deadline =
            Instant::now() + Duration::from_secs(config::SIDECAR_STARTUP_TIMEOUT_SECS);

        log::info!("Waiting for {} at {} ...", name, health_url);

        while Instant::now() < deadline {
            match client.get(&health_url).send() {
                Ok(resp) if resp.status().is_success() => {
                    log::info!("{} is ready.", name);
                    return Ok(());
                }
                _ => {
                    std::thread::sleep(Duration::from_millis(
                        config::SIDECAR_POLL_INTERVAL_MS,
                    ));
                }
            }
        }

        Err(format!(
            "{} did not become ready within {}s",
            name, config::SIDECAR_STARTUP_TIMEOUT_SECS
        ))
    }

    /// Kill all running sidecar processes gracefully.
    pub fn shutdown(&mut self) {
        if let Some(child) = self.whisper_child.take() {
            log::info!("Killing whisper-server...");
            if let Err(e) = child.kill() {
                log::error!("Failed to kill whisper-server: {}", e);
            }
        }
    }
}

impl Drop for SidecarManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}
