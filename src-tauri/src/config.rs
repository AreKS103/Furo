//! Project Furo — Configuration Constants
//!
//! Central tunables for audio, VAD, transcription, and hotkey settings.

// Paths & Model

/// Default GGML model filename (whisper.cpp format, 8-bit quantized).
pub const GGML_MODEL_FILENAME: &str = "ggml-large-v3-turbo-q8_0.bin";

pub const GGML_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin";

/// Silero VAD ONNX model filename.
pub const VAD_MODEL_FILENAME: &str = "silero_vad.onnx";

/// Download URL for Silero VAD v6 ONNX model (~2 MB).
pub const VAD_MODEL_URL: &str =
    "https://github.com/snakers4/silero-vad/raw/master/src/silero_vad/data/silero_vad.onnx";

/// Subdirectory under %APPDATA%/Furo/ where models are stored.
pub const MODELS_SUBDIR: &str = "models";

// Audio Capture

pub const AUDIO_RATE: u32 = 16_000;

pub const AUDIO_CHANNELS: u16 = 1;

/// Frames per buffer (~32 ms at 16 kHz). Silero VAD expects exactly 512 samples.
pub const AUDIO_CHUNK: u32 = 512;

// Silero VAD

/// Speech probability threshold (0.0–1.0). Chunks below this are discarded.
pub const VAD_THRESHOLD: f32 = 0.45;

// Transcription (whisper.cpp sidecar — OpenAI-compatible API)

pub const WHISPER_SERVER_PORT: u16 = 8080;

pub const WHISPER_SERVER_URL: &str = "http://127.0.0.1:8080";

// Note: whisper.cpp server uses dynamic ggml backend loading for GPU;
// the -ngl flag is not supported (that is a llama.cpp flag).
// GPU is enabled automatically via ggml-cuda.dll backend.

pub const LANGUAGE: &str = "en";

/// Initial prompt — biases Whisper toward programming/technical vocabulary and user preferences.
pub const INITIAL_PROMPT: &str = "\
Claude, Furo, FastAPI, React, TypeScript, Tailwind CSS, async/await, kwargs, \
Tauri, CTranslate2, WebSocket, JSON, API, REST, GraphQL, \
npm, pnpm, venv, PyAudio, CUDA, ONNX, int8, float16, \
GitHub, Vercel, Docker, Kubernetes, PostgreSQL, Redis, \
useState, useEffect, useRef, className, onClick, onChange";

// DSP Conditioning

/// Highpass filter cutoff frequency (Hz). Removes rumble/hum below this.
pub const DSP_HIGHPASS_CUTOFF: f32 = 300.0;

/// High-shelf filter cutoff frequency (Hz). Boosts sibilants above this.
pub const DSP_HIGH_SHELF_CUTOFF: f32 = 2500.0;

/// High-shelf gain (dB). Positive = boost.
pub const DSP_HIGH_SHELF_GAIN_DB: f32 = 5.0;

/// Compressor threshold (dBFS).
pub const DSP_COMPRESSOR_THRESHOLD_DB: f32 = -20.0;

/// Compressor ratio (e.g. 3.0 means 3:1).
pub const DSP_COMPRESSOR_RATIO: f32 = 3.0;

// Typing / Text Injection

/// Delay between focus restore and Ctrl+V (or Cmd+V on macOS).
pub const TYPING_FOCUS_DELAY_MS: u64 = 50;

pub const INSERT_TRAILING_SPACE: bool = true;

// Volume Metering

pub const VOLUME_THROTTLE_MS: u64 = 50;

/// Initial noise floor estimate (dBFS) for EMA tracking.
pub const INITIAL_NOISE_FLOOR_DBFS: f32 = -50.0;

/// EMA smoothing factor for noise floor (~1.5s time constant at 50ms intervals).
pub const NOISE_FLOOR_ALPHA: f32 = 0.02;

pub const PEAK_CEILING_DBFS: f32 = -5.0;

// Adaptive Noise Gate

/// Number of audio chunks to collect before applying the adaptive gate.
/// At 512 samples / 16 kHz, one chunk = 32 ms, so 8 chunks ≈ 256 ms of
/// room-noise calibration at the start of each recording.
pub const ADAPTIVE_GATE_WARM_UP_CHUNKS: u32 = 8;

/// How many dB above the tracked noise floor to set the adaptive gate.
/// 10 dB = signal must be ~3× louder than the ambient noise to pass.
pub const ADAPTIVE_GATE_HEADROOM_DB: f32 = 10.0;

// Microphone Filtering

/// Device names containing any of these keywords are excluded from the mic list.
/// Keep this list tight — only loopback/mixer devices that are never real mics.
pub const MIC_EXCLUDE_KEYWORDS: &[&str] = &[
    "stereo mix",
    "what u hear",
    "wave out",
    "loopback",
    "monitor of",
    "screen capture",
];

// Sidecar Health Check

/// Maximum time to wait for a sidecar server to become ready (seconds).
/// Large models can take 60+ seconds on CPU fallback.
pub const SIDECAR_STARTUP_TIMEOUT_SECS: u64 = 120;

pub const SIDECAR_POLL_INTERVAL_MS: u64 = 250;

// Input Profiles

/// Audio input profile — tuned parameters for different microphone placements.
#[derive(Debug, Clone, Copy)]
pub struct InputProfile {
    /// Digital gain applied to raw audio before everything else (dB).
    pub input_gain_db: f32,
    /// Noise gate threshold (dBFS). Samples below this RMS are zeroed.
    pub noise_gate_dbfs: f32,
    /// Highpass filter cutoff applied before VAD and volume metering (Hz).
    pub pre_vad_highpass_hz: f32,
    /// Override for the VAD speech probability threshold (0.0–1.0).
    pub vad_threshold: f32,
    /// Multiplier for the volume meter display value.
    pub volume_display_boost: f32,
}

/// Headset mic: close to mouth, good SNR, minimal processing needed.
pub const PROFILE_HEADSET: InputProfile = InputProfile {
    input_gain_db: 0.0,
    noise_gate_dbfs: -60.0, // effectively off
    pre_vad_highpass_hz: 80.0,
    vad_threshold: VAD_THRESHOLD,
    volume_display_boost: 1.0,
};

/// Laptop mic: far from mouth, low SNR, needs significant boost and filtering.
pub const PROFILE_LAPTOP: InputProfile = InputProfile {
    input_gain_db: 18.0,    // +18 dB to compensate for distance
    noise_gate_dbfs: -45.0,  // reject fan/ambient noise
    pre_vad_highpass_hz: 200.0, // reject case rumble, fan hum
    vad_threshold: 0.35,     // more sensitive due to lower levels
    volume_display_boost: 2.5,
};

/// Look up a profile by name. Returns headset for any unknown name.
pub fn input_profile_by_name(name: &str) -> InputProfile {
    match name {
        "laptop" => PROFILE_LAPTOP,
        _ => PROFILE_HEADSET,
    }
}

/// Suggest a profile name based on device interface type string.
/// "usb" / "bluetooth" → headset, "builtin" → laptop, others → headset.
pub fn suggest_profile_for_interface(interface_type: &str) -> &'static str {
    match interface_type {
        "builtin" => "laptop",
        _ => "headset",
    }
}
