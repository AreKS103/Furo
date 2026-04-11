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

/// Initial prompt — biases Whisper toward programming/technical vocabulary.
pub const INITIAL_PROMPT: &str = "\
FastAPI, React, TypeScript, Tailwind CSS, async/await, kwargs, \
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

// Microphone Filtering

/// Device names containing any of these keywords are excluded from the mic list.
pub const MIC_EXCLUDE_KEYWORDS: &[&str] = &[
    "stereo mix",
    "what u hear",
    "wave out",
    "loopback",
    "monitor of",
    "virtual",
    "video",
    "camera",
    "webcam",
    "screen capture",
    "obs",
    "voicemeeter",
];

// Sidecar Health Check

/// Maximum time to wait for a sidecar server to become ready (seconds).
/// Large models can take 60+ seconds on CPU fallback.
pub const SIDECAR_STARTUP_TIMEOUT_SECS: u64 = 120;

pub const SIDECAR_POLL_INTERVAL_MS: u64 = 250;
