//! Project Furo — Sidecar Process Manager
//!
//! Spawns and manages the pre-compiled whisper.cpp HTTP server
//! as a Tauri sidecar (start, health poll, graceful shutdown).

use std::path::Path;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
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

        // On Windows the sidecar has a .exe extension; on macOS it has none.
        let sidecar_bin = if cfg!(target_os = "windows") {
            "whisper-server.exe"
        } else {
            "whisper-server"
        };
        let sidecar_name: &str =
            if exe_dir.join(sidecar_bin).exists()
                && !exe_dir.join("binaries").join(sidecar_bin).exists()
            {
                // Production (NSIS / .app bundle): binary at root level, no binaries/ subdir
                "whisper-server"
            } else {
                // Dev (build.rs): binary in binaries/ subdir
                "binaries/whisper-server"
            };
        log::info!("whisper-server sidecar name resolved to: {}", sidecar_name);

        // macOS Gatekeeper sets com.apple.quarantine on downloaded .app bundles,
        // which silently kills unsigned embedded binaries (SIGKILL, exit code 9).
        // Strip the quarantine xattr before spawning.
        #[cfg(target_os = "macos")]
        {
            let binary_path = exe_dir.join(sidecar_bin);
            log::info!("Removing quarantine xattr from {:?}", binary_path);
            let _ = std::process::Command::new("xattr")
                .args(["-dr", "com.apple.quarantine"])
                .arg(&binary_path)
                .output();
            // Also strip from the entire app bundle if possible
            if let Some(contents_dir) = exe_dir.parent() {
                if let Some(app_dir) = contents_dir.parent() {
                    let _ = std::process::Command::new("xattr")
                        .args(["-dr", "com.apple.quarantine"])
                        .arg(app_dir)
                        .output();
                }
            }
        }

        let port_str = config::WHISPER_SERVER_PORT.to_string();
        let mut args = vec![
            "--model",
            model_str,
            "--host",
            "127.0.0.1",
            "--port",
            &port_str,
            "--inference-path",
            "/v1/audio/transcriptions",
        ];

        // Flash Attention is supported on both CUDA (Windows) and Metal (macOS).
        args.push("--flash-attn");

        let cmd = app
            .shell()
            .sidecar(sidecar_name)
            .map_err(|e| format!("Failed to create whisper-server sidecar command: {}", e))?
            .args(&args);

        let (mut rx, child) = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn whisper-server: {}", e))?;

        // Drain sidecar stdout/stderr in a background thread.
        // If nobody reads from the pipe, the OS buffer fills up and
        // whisper-server blocks on its next write → deadlocks HTTP responses.
        std::thread::Builder::new()
            .name("whisper-sidecar-drain".into())
            .spawn(move || {
                while let Some(event) = rx.blocking_recv() {
                    match event {
                        CommandEvent::Stdout(line) => {
                            log::debug!("[whisper-server] {}", String::from_utf8_lossy(&line));
                        }
                        CommandEvent::Stderr(line) => {
                            log::warn!("[whisper-server stderr] {}", String::from_utf8_lossy(&line));
                        }
                        CommandEvent::Terminated(_) => break,
                        _ => {}
                    }
                }
                log::info!("whisper-server sidecar stream ended.");
            })
            .ok();

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

        let mut last_log = Instant::now();
        let mut attempt = 0u32;

        while Instant::now() < deadline {
            attempt += 1;
            match client.get(&health_url).send() {
                Ok(resp) if resp.status().is_success() => {
                    log::info!("{} is ready (after {} attempts).", name, attempt);
                    return Ok(());
                }
                Ok(resp) => {
                    // Server is up but not ready (e.g. 503 = loading model)
                    if last_log.elapsed() > Duration::from_secs(5) {
                        log::info!("{} responded with {} — still loading...", name, resp.status());
                        last_log = Instant::now();
                    }
                }
                Err(e) => {
                    if last_log.elapsed() > Duration::from_secs(5) {
                        log::warn!("{} health check failed: {} (attempt {})", name, e, attempt);
                        last_log = Instant::now();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(
                config::SIDECAR_POLL_INTERVAL_MS,
            ));
        }

        Err(format!(
            "{} did not become ready within {}s ({} attempts). \
             On macOS, check System Settings > Privacy & Security. \
             The sidecar binary may have been blocked by Gatekeeper.",
            name, config::SIDECAR_STARTUP_TIMEOUT_SECS, attempt
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
