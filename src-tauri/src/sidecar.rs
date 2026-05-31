//! Project Furo — Sidecar Process Manager
//!
//! Spawns and manages the pre-compiled whisper.cpp HTTP server
//! as a Tauri sidecar (start, health poll, graceful shutdown).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

use crate::config;

#[cfg(target_os = "windows")]
const WINDOWS_CUDA_RUNTIME_VERSION: &str = "cuda-v13";

#[cfg(target_os = "windows")]
const WINDOWS_CUDA_RUNTIME_URL: &str =
    "https://github.com/AreKS103/Furo/releases/download/dependencies-v1/windows-whisper-binaries.zip";

#[cfg(target_os = "windows")]
const WINDOWS_CUDA_RUNTIME_FILES: &[(&str, &str)] = &[
    (
        "cublas64_13.dll",
        "101ae2b98be62704ec96e90a3c49373b76122fc6b502497a7b6fae9ab0f01564",
    ),
    (
        "cublasLt64_13.dll",
        "517b6a69ac9faa7354cffcbd92179aec0cc18a8f6237d36b28d9adfe8c912d8d",
    ),
    (
        "cudart64_13.dll",
        "352ba4ebe61e9a3b171f357a3daf5dd15b6af4a9857673ff893bd6fd2964c075",
    ),
    (
        "ggml-cuda.dll",
        "d607b4a7735637639b1a6541af94c73d63cae0a7cd366bd570125a4c6006e33a",
    ),
];

#[cfg(target_os = "windows")]
fn ensure_windows_cuda_runtime(exe_dir: &Path) -> Result<PathBuf, String> {
    let runtime_dir = dirs::config_dir()
        .ok_or_else(|| "Could not resolve user config directory".to_string())?
        .join("Furo")
        .join("runtime")
        .join(WINDOWS_CUDA_RUNTIME_VERSION);

    std::fs::create_dir_all(&runtime_dir)
        .map_err(|e| format!("Failed to create CUDA runtime directory: {}", e))?;

    if windows_cuda_runtime_valid(&runtime_dir) {
        log::info!("CUDA runtime cache ready: {}", runtime_dir.display());
        prepend_process_path(&runtime_dir)?;
        return Ok(runtime_dir);
    }

    copy_existing_cuda_runtime(exe_dir, &runtime_dir);
    if windows_cuda_runtime_valid(&runtime_dir) {
        log::info!("Migrated CUDA runtime cache: {}", runtime_dir.display());
        prepend_process_path(&runtime_dir)?;
        return Ok(runtime_dir);
    }

    download_windows_cuda_runtime(&runtime_dir)?;
    if !windows_cuda_runtime_valid(&runtime_dir) {
        return Err("Downloaded CUDA runtime failed verification".to_string());
    }

    log::info!("Downloaded CUDA runtime cache: {}", runtime_dir.display());
    prepend_process_path(&runtime_dir)?;
    Ok(runtime_dir)
}

#[cfg(target_os = "windows")]
fn windows_cuda_runtime_valid(runtime_dir: &Path) -> bool {
    WINDOWS_CUDA_RUNTIME_FILES
        .iter()
        .all(|(name, expected_hash)| {
            let path = runtime_dir.join(name);
            match sha256_file_hex(&path) {
                Ok(actual_hash) if actual_hash == *expected_hash => true,
                Ok(actual_hash) => {
                    log::warn!(
                        "CUDA runtime hash mismatch for {}: expected {}, got {}",
                        path.display(),
                        expected_hash,
                        actual_hash
                    );
                    false
                }
                Err(e) => {
                    log::warn!(
                        "CUDA runtime file missing or unreadable: {} ({})",
                        path.display(),
                        e
                    );
                    false
                }
            }
        })
}

#[cfg(target_os = "windows")]
fn copy_existing_cuda_runtime(exe_dir: &Path, runtime_dir: &Path) {
    let candidates = [exe_dir.to_path_buf(), exe_dir.join("binaries")];
    for (name, expected_hash) in WINDOWS_CUDA_RUNTIME_FILES {
        let dst = runtime_dir.join(name);
        if matches!(sha256_file_hex(&dst), Ok(hash) if hash == *expected_hash) {
            continue;
        }

        for source_dir in &candidates {
            let src = source_dir.join(name);
            if !matches!(sha256_file_hex(&src), Ok(hash) if hash == *expected_hash) {
                continue;
            }

            let tmp = runtime_dir.join(format!("{}.tmp", name));
            if let Err(e) = std::fs::copy(&src, &tmp).and_then(|_| {
                let _ = std::fs::remove_file(&dst);
                std::fs::rename(&tmp, &dst)
            }) {
                let _ = std::fs::remove_file(&tmp);
                log::warn!("Failed to migrate CUDA runtime file {}: {}", name, e);
            }
            break;
        }
    }
}

#[cfg(target_os = "windows")]
fn download_windows_cuda_runtime(runtime_dir: &Path) -> Result<(), String> {
    let archive_path = runtime_dir.join("windows-whisper-binaries.zip.download");
    let _ = std::fs::remove_file(&archive_path);

    log::info!("Downloading CUDA runtime from {}", WINDOWS_CUDA_RUNTIME_URL);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(900))
        .build()
        .map_err(|e| format!("Failed to create CUDA runtime HTTP client: {}", e))?;

    let mut response = client
        .get(WINDOWS_CUDA_RUNTIME_URL)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Failed to download CUDA runtime: {}", e))?;

    {
        let mut file = std::fs::File::create(&archive_path)
            .map_err(|e| format!("Failed to create CUDA runtime archive: {}", e))?;
        std::io::copy(&mut response, &mut file)
            .map_err(|e| format!("Failed to write CUDA runtime archive: {}", e))?;
    }

    extract_windows_cuda_runtime(&archive_path, runtime_dir)?;
    let _ = std::fs::remove_file(&archive_path);
    Ok(())
}

#[cfg(target_os = "windows")]
fn extract_windows_cuda_runtime(archive_path: &Path, runtime_dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| format!("Failed to open CUDA runtime archive: {}", e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("Failed to read CUDA runtime archive: {}", e))?;

    for (name, _) in WINDOWS_CUDA_RUNTIME_FILES {
        let mut found = false;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("Failed to read CUDA runtime zip entry: {}", e))?;
            if entry.is_dir() {
                continue;
            }
            let entry_name = entry.name().replace('\\', "/");
            if entry_name.rsplit('/').next() != Some(*name) {
                continue;
            }

            let dst = runtime_dir.join(name);
            let tmp = runtime_dir.join(format!("{}.tmp", name));
            {
                let mut out = std::fs::File::create(&tmp)
                    .map_err(|e| format!("Failed to create CUDA runtime file {}: {}", name, e))?;
                std::io::copy(&mut entry, &mut out)
                    .map_err(|e| format!("Failed to extract CUDA runtime file {}: {}", name, e))?;
            }
            let _ = std::fs::remove_file(&dst);
            std::fs::rename(&tmp, &dst)
                .map_err(|e| format!("Failed to finalize CUDA runtime file {}: {}", name, e))?;
            found = true;
            break;
        }

        if !found {
            return Err(format!("CUDA runtime archive did not contain {}", name));
        }
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn sha256_file_hex(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect())
}

#[cfg(target_os = "windows")]
fn prepend_process_path(path: &Path) -> Result<(), String> {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![path.to_path_buf()];
    paths.extend(std::env::split_paths(&existing));
    let joined = std::env::join_paths(paths)
        .map_err(|e| format!("Failed to build PATH with CUDA runtime: {}", e))?;
    std::env::set_var("PATH", joined);
    Ok(())
}

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
    pub fn start_whisper(&mut self, app: &AppHandle, model_path: &Path) -> Result<(), String> {
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
        let exe_dir = exe_dir
            .parent()
            .ok_or_else(|| "Executable has no parent directory".to_string())?;

        // On Windows the sidecar has a .exe extension; on macOS it has none.
        let sidecar_bin = if cfg!(target_os = "windows") {
            "whisper-server.exe"
        } else {
            "whisper-server"
        };
        let sidecar_name: &str = if exe_dir.join(sidecar_bin).exists()
            && !exe_dir.join("binaries").join(sidecar_bin).exists()
        {
            // Production (NSIS / .app bundle): binary at root level, no binaries/ subdir
            "whisper-server"
        } else {
            // Dev (build.rs): binary in binaries/ subdir
            "binaries/whisper-server"
        };
        log::info!("whisper-server sidecar name resolved to: {}", sidecar_name);

        #[cfg(target_os = "windows")]
        {
            let runtime_dir = ensure_windows_cuda_runtime(exe_dir)?;
            log::info!(
                "whisper-server CUDA runtime PATH entry: {}",
                runtime_dir.display()
            );
        }

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

        // Thread count for whisper.cpp inference.
        //
        // On Windows with CUDA: the encoder runs on GPU, so CPU threads are
        // only used for the decoder. whisper.cpp's default is 4, and using all
        // cores causes thread contention that degrades CUDA performance.
        //
        // On macOS without dedicated GPU (Intel Macs): the encoder runs
        // entirely on CPU, so using all cores provides a meaningful speedup.
        let thread_count = if cfg!(target_os = "windows") {
            // Match whisper.cpp default — avoids CUDA thread contention.
            "4".to_string()
        } else {
            std::thread::available_parallelism()
                .map(|n| n.get().to_string())
                .unwrap_or_else(|_| "4".to_string())
        };

        let mut args = vec![
            "--model",
            model_str,
            "--host",
            "127.0.0.1",
            "--port",
            &port_str,
            "--inference-path",
            "/v1/audio/transcriptions",
            "--threads",
            &thread_count,
            // Greedy decoding with 1 candidate for speed. suppress-nst removes
            // [music]/[applause] hallucination tokens. Temperature fallback is
            // kept enabled — it is the only mechanism that breaks repetition
            // loops when the decoder gets stuck.
            //
            // NOTE: Do NOT add --no-timestamps here. Timestamps provide timing
            // anchors that the decoder uses to advance through the audio. When
            // combined with max_context=0 (sent per-request), disabling them
            // causes a feedback loop where each segment re-decodes the same
            // audio portion, producing 8x+ repeated output.
            "--best-of",
            "1",
            "--beam-size",
            "1",
            "--suppress-nst",
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

        log::info!(
            "whisper-server args: {:?} (flash_attn={})",
            args,
            use_flash_attn
        );

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
                            log::error!("[whisper-server] process terminated: {:?}", payload);
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
        progress_cb: Option<&dyn Fn(&str)>,
    ) -> Result<(), String> {
        let health_url = format!("{}/health", base_url);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| format!("HTTP client build error: {}", e))?;

        let deadline = Instant::now() + Duration::from_secs(config::SIDECAR_STARTUP_TIMEOUT_SECS);
        let start = Instant::now();

        log::info!(
            "Waiting for {} at {} (timeout {}s)...",
            name,
            health_url,
            config::SIDECAR_STARTUP_TIMEOUT_SECS
        );

        let mut last_log = Instant::now();
        let mut attempt = 0u32;

        while Instant::now() < deadline {
            // Short-circuit if the sidecar process already crashed
            if sidecar_exited.load(Ordering::SeqCst) {
                let lines = stderr_capture.lock();
                let tail: Vec<&str> = lines.iter().rev().take(15).map(|s| s.as_str()).collect();
                let tail: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
                let msg = format!(
                    "{} crashed before becoming ready.\nLast stderr output:\n{}",
                    name,
                    if tail.is_empty() {
                        "(no output captured)".to_string()
                    } else {
                        tail
                    }
                );
                log::error!("{}", msg);
                return Err(msg);
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
                        let elapsed = start.elapsed().as_secs();
                        let msg = format!(
                            "{} responded {} — loading model… ({}s / {}s)",
                            name,
                            resp.status(),
                            elapsed,
                            config::SIDECAR_STARTUP_TIMEOUT_SECS
                        );
                        log::info!("{}", msg);
                        if let Some(cb) = &progress_cb {
                            cb(&msg);
                        }
                        last_log = Instant::now();
                    }
                }
                Err(e) => {
                    if last_log.elapsed() > Duration::from_secs(5) {
                        let elapsed = start.elapsed().as_secs();
                        let msg = format!(
                            "{} not reachable yet ({}s / {}s): {}",
                            name,
                            elapsed,
                            config::SIDECAR_STARTUP_TIMEOUT_SECS,
                            e
                        );
                        log::warn!("{}", msg);
                        if let Some(cb) = &progress_cb {
                            cb(&format!("Waiting for whisper server… ({}s)", elapsed));
                        }
                        last_log = Instant::now();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(config::SIDECAR_POLL_INTERVAL_MS));
        }

        Err(format!(
            "{} did not become ready within {}s ({} attempts). \
             On macOS, check System Settings > Privacy & Security. \
             The sidecar binary may have been blocked by Gatekeeper.",
            name,
            config::SIDECAR_STARTUP_TIMEOUT_SECS,
            attempt
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
