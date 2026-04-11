//! Project Furo — Pipeline Orchestrator
//!
//! Central state machine wiring all modules together:
//!   hotkey → audio → VAD → DSP → Whisper → typer → Tauri events

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

use crate::audio::{AudioRecorder, MicInfo};
use crate::config;
use crate::hotkey::{HotkeyCallbacks, HotkeyListener, REBIND_MODE_ACTIVE, WIN_IS_COMBO_MODIFIER};
use crate::processor;
use crate::settings::SettingsStore;
use crate::sidecar::SidecarManager;
use crate::transcriber::Transcriber;
use crate::typer::{self, CapturedTarget};
use crate::vad::VoiceActivityDetector;

// ── Widget activation sound (cpal output)
//
// WinMM's PlaySoundA is a legacy WASAPI thunk. On Windows 10/11 it silently
// returns 0 (failure) when cpal already holds a WASAPI session, because their
// two sessions compete for the audio engine slot. Using cpal for output too
// puts both streams through the same WASAPI shared-mode path, which is
// designed to multiplex many producers without conflict.

static WIDGET_WAV: &[u8] = include_bytes!("../sounds/widget.wav");

/// Spawn a thread that plays the widget activation WAV via cpal output.
/// `volume` is a linear 0.0–1.0 scalar. Returns immediately.
fn play_widget_sound(volume: f32) {
    let _ = std::thread::Builder::new()
        .name("furo-sound".into())
        .spawn(move || {
            if let Err(e) = play_wav_on_output(WIDGET_WAV, volume) {
                log::warn!("play_widget_sound: {}", e);
            }
        });
}

/// Decode a 16-bit PCM WAV and play it on the default output device via cpal.
///
/// Steps:
///   1. Parse the RIFF/WAVE headers to extract raw i16 PCM frames.
///   2. Convert to f32 (WASAPI shared-mode always accepts f32 internally).
///   3. Resample/remix from the WAV's layout to whatever the device prefers.
///   4. Open a cpal output stream, drain all samples, then close cleanly.
fn play_wav_on_output(wav: &'static [u8], volume: f32) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // ── 1. Parse WAV ─────────────────────────────────────────────────────────
    if wav.len() < 44 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err("invalid RIFF/WAVE header".into());
    }
    if &wav[12..16] != b"fmt " {
        return Err("fmt chunk not at expected offset 12".into());
    }
    let audio_fmt = u16::from_le_bytes([wav[20], wav[21]]);
    let wav_ch    = u16::from_le_bytes([wav[22], wav[23]]) as usize;
    let wav_rate  = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
    let bits      = u16::from_le_bytes([wav[34], wav[35]]);
    if audio_fmt != 1 { return Err(format!("non-PCM WAV (audio_format={})", audio_fmt)); }
    if bits != 16      { return Err(format!("need 16-bit PCM, got {} bits", bits)); }

    // Locate the "data" chunk (may follow extra metadata chunks after fmt).
    let fmt_size = u32::from_le_bytes([wav[16], wav[17], wav[18], wav[19]]) as usize;
    let mut off  = 12 + 8 + fmt_size;
    let (data_off, data_sz) = loop {
        if off + 8 > wav.len() { return Err("data chunk not found".into()); }
        let id = &wav[off..off + 4];
        let sz = u32::from_le_bytes([wav[off+4], wav[off+5], wav[off+6], wav[off+7]]) as usize;
        if id == b"data" { break (off + 8, sz); }
        off += 8 + sz;
    };

    // Convert i16 PCM → f32 normalised [-1.0, 1.0]
    let pcm = &wav[data_off..(data_off + data_sz).min(wav.len())];
    let wav_f32: Vec<f32> = pcm
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0)
        .collect();

    // ── 2. Open output device ─────────────────────────────────────────────────
    let host   = cpal::default_host();
    let device = host.default_output_device().ok_or("no default output device")?;
    let cfg    = device.default_output_config().map_err(|e| e.to_string())?;
    let dev_ch   = cfg.channels() as usize;
    let dev_rate = cfg.sample_rate();

    // ── 3. Resample / remix to device native layout, then attenuate ──────────
    let resampled = if wav_rate == dev_rate && wav_ch == dev_ch {
        wav_f32
    } else {
        wav_resample_remix(&wav_f32, wav_ch, wav_rate, dev_ch, dev_rate)
    };
    let samples: Arc<Vec<f32>> = Arc::new(resampled.into_iter().map(|s| s * volume.clamp(0.0, 1.0)).collect());

    // ── 4. Build f32 output stream and play ───────────────────────────────────
    let stream_cfg = cpal::StreamConfig {
        channels:    dev_ch as cpal::ChannelCount,
        sample_rate: dev_rate,
        buffer_size: cpal::BufferSize::Default,
    };
    let pos  = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let (pos_w, done_w, smp_w) = (Arc::clone(&pos), Arc::clone(&done), Arc::clone(&samples));

    let stream = device
        .build_output_stream(
            &stream_cfg,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for s in out.iter_mut() {
                    let p = pos_w.fetch_add(1, Ordering::Relaxed);
                    *s = if p < smp_w.len() {
                        smp_w[p]
                    } else {
                        done_w.store(true, Ordering::Relaxed);
                        0.0
                    };
                }
            },
            |e| log::warn!("sound output stream error: {}", e),
            None,
        )
        .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;

    // Block until all samples are consumed (or 5-second safety timeout).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Give the device one extra buffer period to drain the last chunk.
    std::thread::sleep(std::time::Duration::from_millis(50));
    drop(stream);
    Ok(())
}

/// Linear-interpolation resample + channel remix (averages to mono then fans out).
fn wav_resample_remix(
    src: &[f32],
    src_ch: usize,
    src_rate: u32,
    dst_ch: usize,
    dst_rate: u32,
) -> Vec<f32> {
    let src_frames = src.len() / src_ch.max(1);
    let dst_frames = (src_frames as f64 * dst_rate as f64 / src_rate as f64).round() as usize;
    let mut out = Vec::with_capacity(dst_frames * dst_ch);
    let safe_src_ch = src_ch.max(1);
    for i in 0..dst_frames {
        let pos  = i as f64 * src_rate as f64 / dst_rate as f64;
        let idx  = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let next = (idx + 1).min(src_frames.saturating_sub(1));
        let mix  = |frame: usize| -> f32 {
            (0..safe_src_ch)
                .map(|c| src[(frame * safe_src_ch + c).min(src.len().saturating_sub(1))])
                .sum::<f32>()
                / safe_src_ch as f32
        };
        let mono = mix(idx) + (mix(next) - mix(idx)) * frac;
        for _ in 0..dst_ch {
            out.push(mono);
        }
    }
    out
}

// ── Event payloads

#[derive(Clone, serde::Serialize)]
pub struct StatusPayload {
    pub state: String,
    pub message: String,
}

#[derive(Clone, serde::Serialize)]
pub struct VolumePayload {
    pub level: f64,
}

#[derive(Clone, serde::Serialize)]
pub struct TranscriptionPayload {
    pub text: String,
}

#[derive(Clone, serde::Serialize)]
pub struct ErrorPayload {
    pub message: String,
}

#[derive(Clone, serde::Serialize)]
pub struct DownloadProgressPayload {
    pub progress: f64,
    pub message: String,
}

#[derive(Clone, serde::Serialize)]
pub struct SettingsPayload {
    pub data: std::collections::HashMap<String, String>,
}

// ── Recording mode

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingMode {
    None,
    Hold,
    Handsfree,
}

// ── Pipeline

pub struct FuroPipeline {
    pub settings: SettingsStore,
    pub app_handle: AppHandle,

    models_ready: AtomicBool,
    recording_mode: Mutex<RecordingMode>,

    recorder: Mutex<AudioRecorder>,
    vad: Mutex<Option<VoiceActivityDetector>>,
    transcriber: Mutex<Option<Transcriber>>,
    pub sidecar: Mutex<SidecarManager>,
    hotkey_listener: Mutex<Option<HotkeyListener>>,

    /// Speech-filtered audio chunks (accumulated during recording).
    speech_buffer: Arc<Mutex<VecDeque<Vec<i16>>>>,
    /// Captured target HWND at recording start.
    captured_target: Mutex<Option<CapturedTarget>>,

    models_dir: PathBuf,
}

impl FuroPipeline {
    pub fn new(app_handle: AppHandle, settings: SettingsStore) -> Arc<Self> {
        let models_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Furo")
            .join(config::MODELS_SUBDIR);

        Arc::new(Self {
            settings,
            app_handle,
            models_ready: AtomicBool::new(false),
            recording_mode: Mutex::new(RecordingMode::None),
            recorder: Mutex::new(AudioRecorder::new()),
            vad: Mutex::new(None),
            transcriber: Mutex::new(None),
            sidecar: Mutex::new(SidecarManager::new()),
            hotkey_listener: Mutex::new(None),
            speech_buffer: Arc::new(Mutex::new(VecDeque::new())),
            captured_target: Mutex::new(None),
            models_dir,
        })
    }

    // ── Event emission helpers ───────────────────────────────────

    pub fn emit_status(&self, state: &str, message: &str) {
        let _ = self.app_handle.emit(
            "furo://status",
            StatusPayload {
                state: state.to_string(),
                message: message.to_string(),
            },
        );
    }

    pub fn emit_volume(&self, level: f64) {
        let _ = self.app_handle.emit("furo://volume", VolumePayload { level });
    }

    pub fn emit_transcription(&self, text: &str) {
        let _ = self.app_handle.emit(
            "furo://transcription",
            TranscriptionPayload {
                text: text.to_string(),
            },
        );
    }

    pub fn emit_error(&self, message: &str) {
        let _ = self.app_handle.emit(
            "furo://error",
            ErrorPayload {
                message: message.to_string(),
            },
        );
    }

    pub fn emit_settings(&self) {
        let _ = self.app_handle.emit(
            "furo://settings",
            SettingsPayload {
                data: self.settings.all(),
            },
        );
    }

    pub fn emit_download_progress(&self, progress: f64, message: &str) {
        let _ = self.app_handle.emit(
            "furo://model-download-progress",
            DownloadProgressPayload {
                progress,
                message: message.to_string(),
            },
        );
    }

    pub fn models_dir(&self) -> std::path::PathBuf {
        self.models_dir.clone()
    }

    // ── Model loading ────────────────────────────────────────────

    /// Load VAD, spawn sidecar servers, create HTTP clients. Call from a background thread.
    pub fn load_models(self: &Arc<Self>) {
        log::info!("[pipeline] load_models() starting — OS={} ARCH={}", std::env::consts::OS, std::env::consts::ARCH);
        if let Ok(exe) = std::env::current_exe() {
            log::info!("[pipeline] exe path: {}", exe.display());
        }
        log::info!("[pipeline] models_dir: {}", self.models_dir.display());

        // Load VAD
        self.emit_status("loading", "Downloading VAD model…");
        log::info!("[pipeline] step 1/5: VAD model download/check");
        match VoiceActivityDetector::ensure_model_downloaded(&self.models_dir) {
            Ok(vad_path) => {
                let threshold = self
                    .settings
                    .get("vad_threshold")
                    .parse::<f32>()
                    .unwrap_or(config::VAD_THRESHOLD);

                match VoiceActivityDetector::new(&vad_path, threshold) {
                    Ok(v) => {
                        *self.vad.lock() = Some(v);
                        log::info!("VAD ready.");
                    }
                    Err(e) => {
                        log::error!("Failed to load VAD: {}", e);
                        self.emit_error(&format!("VAD load error: {}", e));
                    }
                }
            }
            Err(e) => {
                log::error!("Failed to download VAD model: {}", e);
                self.emit_error(&format!("VAD download error: {}", e));
            }
        }

        // Download Whisper model if missing
        log::info!("[pipeline] step 2/5: Whisper model download/check");
        self.emit_status("loading", "Checking Whisper model…");
        let pipeline_whisper = Arc::clone(self);
        let whisper_model_path = match Transcriber::ensure_model_downloaded(
            &self.models_dir,
            move |pct, msg| {
                pipeline_whisper.emit_download_progress(pct, msg);
            },
        ) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to download Whisper model: {}", e);
                self.emit_error(&format!("Whisper download error: {}", e));
                return;
            }
        };

        // Spawn whisper-server sidecar
        log::info!("[pipeline] step 3/5: spawning whisper-server sidecar");
        self.emit_status("loading", "Starting whisper server…");
        let (sidecar_exited, stderr_capture) = {
            let mut sidecar = self.sidecar.lock();
            if let Err(e) = sidecar.start_whisper(&self.app_handle, &whisper_model_path) {
                log::error!("Failed to start whisper-server: {}", e);
                self.emit_error(&format!("Whisper server error: {}", e));
                return;
            }
            (
                Arc::clone(&sidecar.sidecar_exited),
                Arc::clone(&sidecar.stderr_capture),
            )
        };

        // Wait for whisper-server to become healthy
        log::info!("[pipeline] step 4/5: waiting for whisper-server health check");
        self.emit_status("loading", "Waiting for whisper server to start…");
        {
            let pipeline_ref = Arc::clone(self);
            let progress_cb = move |msg: &str| {
                pipeline_ref.emit_status("loading", msg);
            };
            if let Err(e) = SidecarManager::wait_for_ready(
                config::WHISPER_SERVER_URL,
                "whisper-server",
                &sidecar_exited,
                &stderr_capture,
                Some(&progress_cb),
            ) {
                log::error!("{}", e);
                // Surface crash details in both the status bar AND error event
                self.emit_status("loading", &format!("Server failed: {}", e.chars().take(120).collect::<String>()));
                self.emit_error(&e);
                return;
            }
        }

        // Create HTTP transcriber client
        log::info!("[pipeline] step 5/5: creating HTTP transcriber + warmup");
        *self.transcriber.lock() = Some(Transcriber::new());
        log::info!("Whisper HTTP transcriber ready.");

        // Warm up CUDA backend — first inference triggers kernel JIT compilation
        // which takes ~90s. Doing it here hides the latency from the user.
        self.emit_status("loading", "Warming up GPU…");
        {
            let mut transcriber = self.transcriber.lock();
            if let Some(ref mut t) = *transcriber {
                t.warmup();
            }
        }
        self.emit_status("loading", "Whisper ready.");

        self.models_ready.store(true, Ordering::SeqCst);

        let hold_hk = self.settings.get("hotkey_hold").to_uppercase();
        let hf_hk = self.settings.get("hotkey_handsfree").to_uppercase();
        self.emit_status(
            "ready",
            &format!("Hold: {} · Hands-free: {}", hold_hk, hf_hk),
        );
        log::info!("All models loaded. Hold: {} | Hands-free: {}", hold_hk, hf_hk);
    }

    // ── Hotkey listener management ───────────────────────────────

    pub fn start_hotkey_listener(self: &Arc<Self>) {
        self.stop_hotkey_listener();

        let hold_str = self.settings.get("hotkey_hold");
        let hf_str = self.settings.get("hotkey_handsfree");

        // Update hook's Win-key suppression flag based on registered combos.
        let win_in_hold = hold_str.split('+').any(|p| matches!(p.trim(), "win" | "cmd"));
        let win_in_hf = hf_str.split('+').any(|p| matches!(p.trim(), "win" | "cmd"));
        WIN_IS_COMBO_MODIFIER.store(win_in_hold || win_in_hf, Ordering::SeqCst);

        let p1 = Arc::clone(self);
        let p2 = Arc::clone(self);
        let p3 = Arc::clone(self);
        let p4 = Arc::clone(self);

        let callbacks = HotkeyCallbacks {
            on_hold_press: Box::new(move || p1.on_hold_press()),
            on_hold_release: Box::new(move || p2.on_hold_release()),
            on_handsfree_press: Box::new(move || p3.on_handsfree_press()),
            on_rebind_captured: Some(Box::new(move |combo: String| {
                // Clear rebind mode so the hook stops suppressing Win key
                REBIND_MODE_ACTIVE.store(false, Ordering::SeqCst);
                let _ = p4.app_handle.emit("furo://rebind-capture", combo);
            })),
        };

        let listener = HotkeyListener::start(&hold_str, &hf_str, callbacks);
        match listener {
            Ok(l) => {
                *self.hotkey_listener.lock() = Some(l);
            }
            Err(e) => {
                log::error!("Hotkey listener failed to start: {}", e);
                self.emit_error(&e);
                let _ = self.app_handle.emit("furo://hotkey-permission", ());
            }
        }
    }

    pub fn stop_hotkey_listener(&self) {
        if let Some(mut listener) = self.hotkey_listener.lock().take() {
            listener.stop();
        }
    }

    // ── Recording lifecycle ──────────────────────────────────────

    pub fn on_hold_press(self: &Arc<Self>) {
        if !self.models_ready.load(Ordering::SeqCst) {
            log::warn!("Hold-press ignored — models still loading.");
            return;
        }
        let mut mode = self.recording_mode.lock();
        if *mode != RecordingMode::None {
            log::warn!("Hold-press ignored — already in {:?} mode.", *mode);
            return;
        }
        *mode = RecordingMode::Hold;
        drop(mode);
        {
            let enabled = self.settings.get("sound_enabled") != "false";
            let vol = self.settings.get("sound_volume").parse::<f32>().unwrap_or(0.05);
            if enabled { play_widget_sound(vol); }
        }
        log::info!("Hold recording started.");
        self.start_recording("Listening…");
    }

    pub fn on_hold_release(self: &Arc<Self>) {
        let mut mode = self.recording_mode.lock();
        if *mode != RecordingMode::Hold {
            return;
        }
        *mode = RecordingMode::None;
        drop(mode);
        log::info!("Hold recording stopped — processing.");
        self.stop_and_process();
    }

    fn on_handsfree_press(self: &Arc<Self>) {
        if !self.models_ready.load(Ordering::SeqCst) {
            log::warn!("Hands-free press ignored — models still loading.");
            return;
        }
        {
            let enabled = self.settings.get("sound_enabled") != "false";
            let vol = self.settings.get("sound_volume").parse::<f32>().unwrap_or(0.05);
            if enabled { play_widget_sound(vol); }
        }
        let mut mode = self.recording_mode.lock();
        if *mode != RecordingMode::None {
            // Toggle off
            *mode = RecordingMode::None;
            drop(mode);
            log::info!("Hands-free stopped — processing.");
            self.stop_and_process();
        } else {
            *mode = RecordingMode::Handsfree;
            drop(mode);
            log::info!("Hands-free recording started.");
            self.start_recording("Hands-free…");
        }
    }

    fn start_recording(self: &Arc<Self>, label: &str) {
        // Capture target window
        *self.captured_target.lock() = typer::capture_target();

        // Reset VAD
        if let Some(ref vad) = *self.vad.lock() {
            vad.reset();
        }

        // Clear speech buffer
        self.speech_buffer.lock().clear();

        // Start audio capture
        let mic_name = self.settings.get("microphone");

        let speech_buf = Arc::clone(&self.speech_buffer);
        let vad_ref = Arc::clone(self);

        let pipeline_vol = Arc::clone(self);

        let mut recorder = self.recorder.lock();
        if let Err(e) = recorder.start(
            &mic_name,
            // on_volume callback
            move |level| {
                pipeline_vol.emit_volume(level as f64);
            },
            // on_raw_chunk callback — local buffering state machine
            {
                // All mutable state lives inside a Mutex because cpal's
                // callback signature is Fn (not FnMut).
                struct VadState {
                    sample_acc: Vec<i16>,
                    hangover_chunks: usize,
                    pre_roll: VecDeque<Vec<i16>>,
                    speech_active: bool,
                }
                let state = Mutex::new(VadState {
                    sample_acc: Vec::with_capacity(1024),
                    hangover_chunks: 0,
                    pre_roll: VecDeque::with_capacity(4),
                    speech_active: false,
                });

                move |chunk: &[i16]| {
                    let mut s = state.lock();

                    // Push all incoming samples into the accumulator.
                    s.sample_acc.extend_from_slice(chunk);

                    // Drain exact 512-sample frames and run VAD on each.
                    while s.sample_acc.len() >= 512 {
                        let frame: Vec<i16> = s.sample_acc.drain(..512).collect();

                        let is_speech = {
                            let mut vad_guard = vad_ref.vad.lock();
                            if let Some(ref mut vad) = *vad_guard {
                                vad.is_speech(&frame)
                            } else {
                                true // No VAD available — pass all audio through
                            }
                        };

                        if is_speech {
                            // On speech onset, flush pre-roll so leading
                            // consonants are not lost.
                            if !s.speech_active {
                                s.speech_active = true;
                                let mut buf = speech_buf.lock();
                                for old_frame in s.pre_roll.drain(..) {
                                    buf.push_back(old_frame);
                                }
                            }
                            s.hangover_chunks = 15;
                            speech_buf.lock().push_back(frame);
                        } else if s.hangover_chunks > 0 {
                            // Still within hangover window — keep the audio.
                            s.hangover_chunks -= 1;
                            speech_buf.lock().push_back(frame);
                            if s.hangover_chunks == 0 {
                                // Hangover expired — next speech onset will
                                // flush pre-roll again.
                                s.speech_active = false;
                            }
                        } else {
                            // Silence — accumulate in pre-roll ring buffer.
                            if s.pre_roll.len() >= 3 {
                                s.pre_roll.pop_front();
                            }
                            s.pre_roll.push_back(frame);
                        }
                    }
                }
            },
        ) {
            log::error!("Failed to start recording: {}", e);
            self.emit_error(&format!("Recording error: {}", e));
            *self.recording_mode.lock() = RecordingMode::None;
            return;
        }

        self.emit_status("recording", label);
        log::info!("Recording started: {}", label);
    }

    fn stop_and_process(self: &Arc<Self>) {
        // Stop recording
        {
            let mut recorder = self.recorder.lock();
            recorder.stop();
        }

        self.emit_status("processing", "Processing…");
        log::info!("Recording stopped. Processing…");

        // Process audio in a background thread
        let pipeline = Arc::clone(self);
        std::thread::Builder::new()
            .name("furo-process".into())
            .spawn(move || {
                pipeline.process_audio();
            })
            .expect("Failed to spawn processing thread");
    }

    fn process_audio(self: &Arc<Self>) {
        let t0 = std::time::Instant::now();

        // Drain the speech buffer directly into a flat contiguous buffer.
        // Avoids the intermediate Vec<Vec<i16>> allocation.
        let audio_i16: Vec<i16> = {
            let mut buf = self.speech_buffer.lock();
            let total: usize = buf.iter().map(|c| c.len()).sum();
            if total == 0 {
                buf.clear();
                log::info!("VAD: no speech detected.");
                self.emit_transcription("");
                self.emit_status("idle", "");
                return;
            }
            let mut flat = Vec::with_capacity(total);
            for chunk in buf.drain(..) {
                flat.extend_from_slice(&chunk);
            }
            flat
        };

        let speech_s = audio_i16.len() as f64 / config::AUDIO_RATE as f64;
        log::info!("[perf] buffer flatten: {:.1}ms, {:.1}s speech", t0.elapsed().as_secs_f64() * 1000.0, speech_s);

        // Transcribe
        let language = self.settings.get("language");
        let t_pre = t0.elapsed();
        let text = {
            let mut transcriber = self.transcriber.lock();
            if let Some(ref mut t) = *transcriber {
                t.transcribe(&audio_i16, &language)
            } else {
                log::error!("Transcriber not loaded!");
                self.emit_error("Transcriber not loaded.");
                self.emit_status("idle", "");
                return;
            }
        };
        let t_transcribe = t0.elapsed();

        // Post-process through Rust regex rules (no LLM)
        let text = processor::process(&text);
        let t_post = t0.elapsed();
        log::info!("[perf] pipeline: flatten {:.1}ms + transcribe {:.1}ms + postproc {:.1}ms = {:.1}ms total",
            t_pre.as_secs_f64() * 1000.0,
            (t_transcribe - t_pre).as_secs_f64() * 1000.0,
            (t_post - t_transcribe).as_secs_f64() * 1000.0,
            t_post.as_secs_f64() * 1000.0,
        );

        if !text.is_empty() {
            self.emit_transcription(&text);

            // Type text into target window (or fall back to clipboard when
            // Furo itself was the foreground app at the time of the hotkey).
            let target = self.captured_target.lock().clone();
            if let Some(ref target) = target {
                std::thread::sleep(std::time::Duration::from_millis(20));
                let success = typer::type_text(&text, target);
                if !success {
                    self.emit_error("Target window was closed — text copied to clipboard.");
                }
            } else {
                // No external target — transcription is visible in the UI only.
                log::info!("No external target; transcription shown in UI only.");
            }
        } else {
            self.emit_transcription("");
        }

        // Return to idle — only after text is fully injected
        let mode = *self.recording_mode.lock();
        if mode == RecordingMode::None {
            let hold_hk = self.settings.get("hotkey_hold").to_uppercase();
            let hf_hk = self.settings.get("hotkey_handsfree").to_uppercase();
            self.emit_status(
                "ready",
                &format!("Hold: {} · Hands-free: {}", hold_hk, hf_hk),
            );
        }
    }

    // ── Public API (called from Tauri commands) ──────────────────

    pub fn list_microphones(&self) -> Vec<MicInfo> {
        AudioRecorder::list_devices()
    }

    pub fn is_ready(&self) -> bool {
        self.models_ready.load(Ordering::SeqCst)
    }

    pub fn current_mode(&self) -> RecordingMode {
        *self.recording_mode.lock()
    }

    /// Preview the widget sound at the currently saved volume (called from Settings).
    pub fn preview_sound(&self) {
        let enabled = self.settings.get("sound_enabled") != "false";
        let vol = self.settings.get("sound_volume").parse::<f32>().unwrap_or(0.05);
        if enabled {
            play_widget_sound(vol);
        }
    }

    /// Gracefully shut down sidecar servers.
    pub fn shutdown(&self) {
        self.sidecar.lock().shutdown();
    }
}
