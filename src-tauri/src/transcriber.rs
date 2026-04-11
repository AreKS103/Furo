//! Project Furo — Transcription Engine (HTTP → whisper.cpp server)
//!
//! Sends audio via HTTP multipart POST to the whisper.cpp sidecar's
//! OpenAI-compatible `/v1/audio/transcriptions` endpoint.

use once_cell::sync::Lazy;
use regex::Regex;
use std::path::PathBuf;
use std::time::Instant;

use crate::config;
use crate::dsp::DspChain;

// ============================================================================
// Whisper model scanner
// ============================================================================

/// A discovered Whisper GGML model on the local system.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FoundModel {
    pub path: String,
    pub label: String,
    pub size_bytes: u64,
}

/// GGML file magic bytes (little-endian u32 stored as bytes).
/// whisper.cpp models use "ggml" (old) or "ggjt" (v3) magic.
const GGML_MAGIC: &[u8] = b"ggml";
const GGJT_MAGIC: &[u8] = b"ggjt";
const GGUF_MAGIC: &[u8] = b"GGUF"; // newer GGUF format

/// Return true if the file starts with a known GGML/GGUF magic signature
/// and is large enough to be a real model (≥ 10 MB).
fn is_valid_whisper_model(path: &std::path::Path) -> bool {
    let Ok(meta) = path.metadata() else { return false };
    if meta.len() < 10 * 1024 * 1024 { return false; } // < 10 MB → skip

    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut magic = [0u8; 4];
    if f.read_exact(&mut magic).is_err() { return false; }

    // Compare in little-endian: the u32 0x67676d6c is stored as bytes [6c 6d 67 67]
    // but File::read gives us the actual bytes on disk.
    // whisper.cpp writes: fout.write((char *) &GGML_FILE_MAGIC, sizeof(magic))
    // where GGML_FILE_MAGIC = 0x67676d6c, so bytes on disk = [6c, 6d, 67, 67].
    let reversed: Vec<u8> = magic.iter().rev().cloned().collect();
    reversed == GGML_MAGIC
        || reversed == GGJT_MAGIC
        || &magic == GGUF_MAGIC  // GGUF is big-endian identifier string
        || &magic[..4] == b"ggml"
        || &magic[..4] == b"ggjt"
}

/// Collect .bin files from a directory that look like GGML Whisper models.
/// Only scans one level deep to avoid slow traversal.
fn scan_dir_for_models(dir: &std::path::Path, out: &mut Vec<FoundModel>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("bin") { continue; }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
        // Accept any file named ggml-* or whisper-*, or validate magic bytes
        let looks_like_whisper = name.starts_with("ggml-") || name.starts_with("whisper-");
        if !looks_like_whisper && !is_valid_whisper_model(&path) { continue; }
        if let Ok(meta) = path.metadata() {
            if meta.len() < 10 * 1024 * 1024 { continue; }
            let size_mb = meta.len() as f64 / 1_048_576.0;
            let label = format!(
                "{} ({:.0} MB)",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown"),
                size_mb
            );
            out.push(FoundModel {
                path: path.to_string_lossy().into_owned(),
                label,
                size_bytes: meta.len(),
            });
        }
    }
}

/// Scan the system for existing GGML-format Whisper models.
///
/// Searches (in order):
/// 1. Furo's own models directory
/// 2. `WHISPER_MODEL` env var (direct file path)
/// 3. `WHISPER_MODELS_DIR` env var (directory)
/// 4. Known whisper.cpp install directories
/// 5. Common download directories
///
/// Results are sorted largest-first (bigger = higher quality model).
pub fn scan_for_whisper_models(models_dir: &PathBuf) -> Vec<FoundModel> {
    let mut found: Vec<FoundModel> = Vec::new();

    // 1. Furo's own models dir
    scan_dir_for_models(models_dir, &mut found);

    // 2. WHISPER_MODEL env var — direct path to a specific model file
    if let Ok(p) = std::env::var("WHISPER_MODEL") {
        let path = std::path::Path::new(&p);
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("bin") {
            if let Ok(meta) = path.metadata() {
                if meta.len() >= 10 * 1024 * 1024 {
                    let size_mb = meta.len() as f64 / 1_048_576.0;
                    found.push(FoundModel {
                        path: p.clone(),
                        label: format!(
                            "{} ({:.0} MB) [WHISPER_MODEL env]",
                            path.file_name().and_then(|n| n.to_str()).unwrap_or("model"),
                            size_mb
                        ),
                        size_bytes: meta.len(),
                    });
                }
            }
        }
    }

    // 3. WHISPER_MODELS_DIR env var
    if let Ok(dir) = std::env::var("WHISPER_MODELS_DIR") {
        scan_dir_for_models(std::path::Path::new(&dir), &mut found);
    }

    // 4. Well-known paths where whisper.cpp models live
    let home = dirs::home_dir().unwrap_or_default();
    let appdata = dirs::config_dir().unwrap_or_default(); // %APPDATA% on Windows
    let local_data = dirs::data_local_dir().unwrap_or_default(); // %LOCALAPPDATA%
    let cache = dirs::cache_dir().unwrap_or_default();

    let search_dirs: &[std::path::PathBuf] = &[
        // whisper.cpp clones / installs
        home.join("whisper.cpp").join("models"),
        home.join("whisper").join("models"),
        appdata.join("whisper.cpp").join("models"),
        local_data.join("whisper.cpp").join("models"),
        std::path::PathBuf::from("C:\\whisper.cpp\\models"),
        std::path::PathBuf::from("C:\\AI\\models"),
        std::path::PathBuf::from("C:\\AI\\whisper"),
        // HuggingFace snapshot cache — whisper.cpp hub downloads
        cache.join("huggingface").join("hub"),
        // Common personal/project folders on Windows
        home.join("Downloads"),
        home.join("Documents").join("AI").join("models"),
        home.join("Documents").join("whisper"),
    ];

    for dir in search_dirs {
        scan_dir_for_models(dir, &mut found);
    }

    // Deduplicate by canonical path
    let mut seen = std::collections::HashSet::new();
    found.retain(|m| seen.insert(m.path.to_lowercase()));

    // Sort: largest file first (largest model = highest quality)
    found.sort_unstable_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    found
}

// Regex for stripping filler words and stuttered doubles
static FILLER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(uh+|um+|er+|ah+|hm+|hmm+|like,?)\b").unwrap()
});
static STUTTER_RE: Lazy<fancy_regex::Regex> = Lazy::new(|| {
    fancy_regex::Regex::new(r"(?i)\b(\w+)\s+\1\b").unwrap()
});
static MULTI_SPACE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"  +").unwrap());

/// HTTP-based transcription client talking to whisper.cpp sidecar server.
pub struct Transcriber {
    client: reqwest::blocking::Client,
    endpoint: String,
    dsp: DspChain,
}

impl Transcriber {
    pub fn new() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .tcp_nodelay(true)        // disable Nagle — shaves ~0-40ms on localhost
            .pool_idle_timeout(std::time::Duration::from_secs(300)) // keep connection warm
            .build()
            .expect("Failed to build HTTP client");

        let endpoint = format!(
            "{}/v1/audio/transcriptions",
            config::WHISPER_SERVER_URL
        );

        let dsp = DspChain::new();
        log::info!("Transcriber ready (HTTP → {})", endpoint);

        Self {
            client,
            endpoint,
            dsp,
        }
    }

    /// Transcribe raw PCM int16 audio to text via whisper.cpp server.
    ///
    /// Pipeline: i16 PCM → f32 → DSP conditioning → WAV encode → HTTP POST → filler strip.
    pub fn transcribe(&mut self, audio_i16: &[i16], language: &str) -> String {
        if audio_i16.is_empty() {
            log::warn!("Empty audio buffer — nothing to transcribe.");
            return String::new();
        }

        let t0 = Instant::now();

        // Convert i16 → f32 in [-1.0, 1.0]
        let mut audio_f32: Vec<f32> = audio_i16
            .iter()
            .map(|&s| s as f32 / 32768.0)
            .collect();

        // DSP conditioning: highpass → high-shelf → compressor
        self.dsp.process(&mut audio_f32);

        let duration_s = audio_f32.len() as f64 / config::AUDIO_RATE as f64;
        let t_dsp = t0.elapsed();
        log::info!("[perf] DSP: {:.1}ms for {:.2}s audio", t_dsp.as_secs_f64() * 1000.0, duration_s);

        let wav_data = encode_wav_f32(&audio_f32, config::AUDIO_RATE);
        // Drop the f32 buffer immediately — no longer needed, frees memory before HTTP
        drop(audio_f32);
        let t_encode = t0.elapsed();

        let file_part = reqwest::blocking::multipart::Part::bytes(wav_data)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .unwrap();

        // Use empty string for auto-detect, otherwise use the language code
        let lang_value = if language == "auto" { "" } else { language };

        let form = reqwest::blocking::multipart::Form::new()
            .part("file", file_part)
            .text("model", "whisper-1") // required by OpenAI compat API, value ignored
            .text("language", lang_value.to_string())
            .text("response_format", "text")
            .text("prompt", config::INITIAL_PROMPT.to_string());

        let t_send = t0.elapsed();
        log::info!("[perf] encode+form: {:.1}ms", (t_send - t_dsp).as_secs_f64() * 1000.0);

        let response = match self.client.post(&self.endpoint).multipart(form).send() {
            Ok(r) => r,
            Err(e) => {
                log::error!("Whisper HTTP request failed: {:#}", e);
                return String::new();
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            log::error!("Whisper server error {}: {}", status, body);
            return String::new();
        }

        let mut text = match response.text() {
            Ok(t) => t.trim().to_string(),
            Err(e) => {
                log::error!("Failed to read whisper response: {}", e);
                return String::new();
            }
        };

        let t_http = t0.elapsed();
        log::info!("[perf] HTTP round-trip: {:.1}ms", (t_http - t_send).as_secs_f64() * 1000.0);

        // Post-processing: strip fillers and stutters
        text = FILLER_RE.replace_all(&text, "").to_string();
        text = STUTTER_RE.replace_all(&text, "$1").to_string();
        text = MULTI_SPACE_RE.replace_all(&text, " ").to_string();
        text = text.trim().to_string();

        let elapsed = t0.elapsed();
        let rtf = elapsed.as_secs_f64() / duration_s.max(0.01);
        let preview = if text.len() > 120 {
            format!("{}...", &text[..120])
        } else {
            text.clone()
        };
        log::info!(
            "[perf] total={:.2}s (DSP {:.1}ms + encode {:.1}ms + HTTP {:.1}ms + post {:.1}ms) RTF {:.2}x: {}",
            elapsed.as_secs_f64(),
            t_dsp.as_secs_f64() * 1000.0,
            (t_encode - t_dsp).as_secs_f64() * 1000.0,
            (t_http - t_encode).as_secs_f64() * 1000.0,
            (elapsed - t_http).as_secs_f64() * 1000.0,
            rtf,
            preview
        );

        text
    }

    /// Send a tiny silent audio clip to warm up the CUDA backend.
    /// The first inference triggers CUDA kernel JIT compilation which can take ~90s.
    /// Calling this during startup makes the first real transcription fast.
    pub fn warmup(&mut self) {
        log::info!("Warming up whisper CUDA backend (first inference may take a while)...");
        let silence = vec![0i16; config::AUDIO_RATE as usize / 10]; // 0.1s silence
        let _ = self.transcribe(&silence, "en");
        log::info!("Whisper CUDA warmup complete.");
    }

    /// Ensure the GGML Whisper model file exists locally.
    ///
    /// Resolution order:
    /// 1. Exact default model   → `models_dir/ggml-large-v3-turbo-q8_0.bin`
    /// 2. System scan           → search known paths for any GGML Whisper model
    /// 3. Download from HuggingFace (last resort, ~900 MB)
    pub fn ensure_model_downloaded(
        models_dir: &PathBuf,
        on_progress: impl Fn(f64, &str),
    ) -> Result<PathBuf, String> {
        // 1. Already have the exact default model?
        let model_path = models_dir.join(config::GGML_MODEL_FILENAME);
        if model_path.exists() {
            log::info!("Whisper model already present at {}", model_path.display());
            return Ok(model_path);
        }

        // 2. Scan the system for any compatible GGML model
        on_progress(0.0, "Scanning system for existing Whisper models...");
        let found = scan_for_whisper_models(models_dir);
        if let Some(best) = found.into_iter().next() {
            log::info!(
                "Found existing Whisper model on system: {} ({} MB)",
                best.path,
                best.size_bytes / 1_048_576
            );
            on_progress(100.0, &format!("Using existing model: {}", best.label));
            return Ok(PathBuf::from(best.path));
        }

        // 3. Nothing found — download the default model
        std::fs::create_dir_all(models_dir)
            .map_err(|e| format!("Failed to create models directory: {}", e))?;

        log::info!(
            "Downloading Whisper model from {}...",
            config::GGML_MODEL_URL
        );
        on_progress(0.0, "Downloading Whisper model...");

        let response = reqwest::blocking::get(config::GGML_MODEL_URL)
            .map_err(|e| format!("Failed to download Whisper model: {}", e))?;

        if !response.status().is_success() {
            return Err(format!(
                "Model download failed with status: {}",
                response.status()
            ));
        }

        let total_size = response.content_length().unwrap_or(0);
        let mut downloaded: u64 = 0;

        let tmp_path = model_path.with_extension("bin.tmp");
        let mut file = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("Failed to create temp file: {}", e))?;

        use std::io::{Read, Write};
        let mut reader = response;
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("Download read error: {}", e))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])
                .map_err(|e| format!("Write error: {}", e))?;
            downloaded += n as u64;

            if total_size > 0 {
                let pct = (downloaded as f64 / total_size as f64) * 100.0;
                let mb = downloaded as f64 / 1_048_576.0;
                let total_mb = total_size as f64 / 1_048_576.0;
                on_progress(
                    pct,
                    &format!("Downloading model: {:.0}/{:.0} MB ({:.1}%)", mb, total_mb, pct),
                );
            }
        }

        std::fs::rename(&tmp_path, &model_path)
            .map_err(|e| format!("Failed to rename temp file: {}", e))?;

        log::info!(
            "Whisper model downloaded ({} bytes) to {}",
            downloaded,
            model_path.display()
        );
        on_progress(100.0, "Model download complete.");

        Ok(model_path)
    }
}

/// Encode f32 PCM samples as a 16-bit mono WAV in memory.
fn encode_wav_f32(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let num_samples = samples.len() as u32;
    let bytes_per_sample: u16 = 2; // 16-bit
    let data_size = num_samples * bytes_per_sample as u32;
    let file_size = 36 + data_size; // RIFF header sans 8 bytes

    let mut buf = Vec::with_capacity(file_size as usize + 8);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt subchunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // subchunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * bytes_per_sample as u32).to_le_bytes()); // byte rate
    buf.extend_from_slice(&bytes_per_sample.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data subchunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());

    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let i16_val = (clamped * 32767.0) as i16;
        buf.extend_from_slice(&i16_val.to_le_bytes());
    }

    buf
}
