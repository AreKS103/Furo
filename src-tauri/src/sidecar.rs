//! Project Furo — Sidecar Process Manager
//!
//! Spawns and manages the pre-compiled whisper.cpp HTTP server
//! as a Tauri sidecar (start, health poll, graceful shutdown).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

use crate::config;

/// Running sidecar server handle.
pub struct SidecarManager {
    whisper_child: Option<CommandChild>,
    /// Set to `true` when the sidecar process terminates (crash or normal exit).
    pub sidecar_exited: Arc<AtomicBool>,
    /// Last stderr lines from the sidecar (for crash diagnostics).
    pub stderr_capture: Arc<parking_lot::Mutex<Vec<String>>>,
}

impl SidecarManager {
    pub fn new() -> Self {
        Self {
            whisper_child: None,
            sidecar_exited: Arc::new(AtomicBool::new(false)),
            stderr_capture: Arc::new(parking_lot::Mutex::new(Vec::new())),
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

        // Flash Attention works with CUDA (Windows) and Metal on Apple Silicon.
        // On Intel Macs (x86_64), Metal flash attention kernels can crash with
        // AMD/Intel GPUs, so only enable on arm64 macOS and all of Windows.
        let use_flash_attn = if cfg!(target_os = "macos") {
            std::env::consts::ARCH == "aarch64"
        } else {
            true
        };
        if use_flash_attn {
            args.push("--flash-attn");
        }
        log::info!("whisper-server args: {:?} (flash_attn={})", args, use_flash_attn);

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
        let exited_flag = Arc::clone(&self.sidecar_exited);
        let stderr_buf = Arc::clone(&self.stderr_capture);
        // Reset shared state for fresh spawn
        self.sidecar_exited.store(false, Ordering::SeqCst);
        self.stderr_capture.lock().clear();

        std::thread::Builder::new()
            .name("whisper-sidecar-drain".into())
            .spawn(move || {
                while let Some(event) = rx.blocking_recv() {
                    match event {
                        CommandEvent::Stdout(line) => {
                            log::debug!("[whisper-server] {}", String::from_utf8_lossy(&line));
                        }
                        CommandEvent::Stderr(line) => {
                            let text = String::from_utf8_lossy(&line).to_string();
                            log::warn!("[whisper-server stderr] {}", text);
                            let mut buf = stderr_buf.lock();
                            buf.push(text);
                            if buf.len() > 50 {
                                buf.remove(0);
                            }
                        }
                        CommandEvent::Terminated(payload) => {
                            log::error!(
                                "[whisper-server] process terminated: {:?}",
                                payload
                            );
                            exited_flag.store(true, Ordering::SeqCst);
                            break;
                        }
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
    pub fn wait_for_ready(
        base_url: &str,
        name: &str,
        sidecar_exited: &AtomicBool,
        stderr_capture: &parking_lot::Mutex<Vec<String>>,
    ) -> Result<(), String> {
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
            // Short-circuit if the sidecar process already crashed
            if sidecar_exited.load(Ordering::SeqCst) {
                let lines = stderr_capture.lock();
                let tail: Vec<&str> = lines.iter().rev().take(15).map(|s| s.as_str()).collect();
                let tail: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
                return Err(format!(
                    "{} crashed before becoming ready.\nLast stderr output:\n{}",
                    name,
                    if tail.is_empty() {
                        "(no output captured)".to_string()
                    } else {
                        tail
                    }
                ));
            }

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
