//! Project Furo — Voice Activity Detection (Silero VAD via ONNX Runtime)
//!
//! Wraps the Silero VAD v6 ONNX model to classify 16 kHz audio chunks as
//! speech or silence. Only chunks that contain speech are forwarded to the
//! transcription engine, preventing Whisper hallucinations on silence and
//! saving GPU cycles.
//!
//! Uses `ort` (ONNX Runtime) with CPU execution provider — GPU VRAM is
//! reserved exclusively for Whisper inference.
//! Port of Python `vad.py`.

use ort::session::Session;
use ort::value::Tensor;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

use crate::config;

/// Streaming Silero VAD wrapper using ONNX Runtime.
///
/// Silero VAD expects fixed-size audio windows (512 samples at 16 kHz).
/// The model is stateful (recurrent) so it must be `reset()` between
/// separate recordings.
pub struct VoiceActivityDetector {
    session: Session,
    threshold: f32,
    sample_rate: i64,
    /// Unified hidden state: shape [2, 1, 128] (Silero VAD v6)
    state: Arc<Mutex<Vec<f32>>>,
    /// Context buffer: last 64 samples from previous chunk (Silero VAD v6)
    context: Arc<Mutex<Vec<f32>>>,
}

impl VoiceActivityDetector {
    /// Load the Silero VAD ONNX model from disk.
    ///
    /// The model file is expected at `model_path`. If it doesn't exist,
    /// it should be downloaded first via `ensure_model_downloaded()`.
    pub fn new(model_path: &PathBuf, threshold: f32) -> Result<Self, String> {
        if !model_path.exists() {
            return Err(format!(
                "VAD model not found at {}. Call ensure_model_downloaded() first.",
                model_path.display()
            ));
        }

        let session = Session::builder()
            .map_err(|e| format!("Failed to create ONNX session builder: {}", e))?
            .with_intra_threads(1)
            .map_err(|e| format!("Failed to set intra threads: {}", e))?
            .commit_from_file(model_path)
            .map_err(|e| format!("Failed to load VAD model: {}", e))?;

        log::info!("Silero VAD loaded from {}", model_path.display());

        for inp in session.inputs() {
            log::debug!("VAD model input: {}", inp.name());
        }
        for out in session.outputs() {
            log::debug!("VAD model output: {}", out.name());
        }

        Ok(Self {
            session,
            threshold,
            sample_rate: config::AUDIO_RATE as i64,
            state: Arc::new(Mutex::new(vec![0.0_f32; 2 * 1 * 128])),
            context: Arc::new(Mutex::new(vec![0.0_f32; 64])),
        })
    }

    /// Reset the model's internal recurrent state between recordings.
    pub fn reset(&self) {
        self.state.lock().fill(0.0);
        self.context.lock().fill(0.0);
    }

    /// Evaluate whether `chunk_i16` contains human speech.
    ///
    /// The chunk should be exactly `config::AUDIO_CHUNK` (512) i16 samples.
    /// Returns `true` if speech probability exceeds the configured threshold.
    pub fn is_speech(&mut self, chunk_i16: &[i16]) -> bool {
        // Convert int16 → float32 in [-1.0, 1.0]
        let chunk_f32: Vec<f32> = chunk_i16
            .iter()
            .map(|&s| s as f32 / 32768.0)
            .collect();

        // Silero VAD v6: prepend 64-sample context to 512-sample chunk → 576 total
        let context = self.context.lock().clone();
        let mut input_data = Vec::with_capacity(context.len() + chunk_f32.len());
        input_data.extend_from_slice(&context);
        input_data.extend_from_slice(&chunk_f32);

        // Save last 64 samples as context for next call
        {
            let mut ctx = self.context.lock();
            let total = input_data.len();
            ctx.clear();
            ctx.extend_from_slice(&input_data[total.saturating_sub(64)..]);
        }

        let input_len = input_data.len();

        // sr: [1] — sample rate as i64
        let sr_data = vec![self.sample_rate];

        // Get current unified state
        let state_data = self.state.lock().clone();

        // Create ort Tensor values
        let input_tensor = match Tensor::from_array(([1usize, input_len], input_data.into_boxed_slice())) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Failed to create input tensor: {}", e);
                return false;
            }
        };
        let sr_tensor = match Tensor::from_array(([1usize], sr_data.into_boxed_slice())) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Failed to create sr tensor: {}", e);
                return false;
            }
        };
        let state_tensor = match Tensor::from_array(([2usize, 1, 128], state_data.into_boxed_slice())) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Failed to create state tensor: {}", e);
                return false;
            }
        };

        // Run inference — Silero VAD v6 inputs: "input", "state", "sr"
        let result = match self.session.run(
            ort::inputs![
                "input" => input_tensor,
                "state" => state_tensor,
                "sr" => sr_tensor,
            ],
        ) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("VAD inference failed: {}", e);
                return false;
            }
        };

        // Extract output probability (first output: "output")
        let speech_prob = match result["output"].try_extract_tensor::<f32>() {
            Ok((_, data)) => data[0],
            Err(e) => {
                log::warn!("Failed to extract VAD output: {}", e);
                return false;
            }
        };

        // Update unified state (second output: "stateN")
        if let Ok((_, data)) = result["stateN"].try_extract_tensor::<f32>() {
            *self.state.lock() = data.to_vec();
        }

        speech_prob >= self.threshold
    }

    /// Ensure the Silero VAD ONNX model file exists locally.
    /// Downloads it from GitHub if missing.
    pub fn ensure_model_downloaded(models_dir: &PathBuf) -> Result<PathBuf, String> {
        let model_path = models_dir.join(config::VAD_MODEL_FILENAME);

        if model_path.exists() {
            log::info!("VAD model already present at {}", model_path.display());
            return Ok(model_path);
        }

        std::fs::create_dir_all(models_dir)
            .map_err(|e| format!("Failed to create models directory: {}", e))?;

        log::info!("Downloading Silero VAD model from {}...", config::VAD_MODEL_URL);

        let response = reqwest::blocking::get(config::VAD_MODEL_URL)
            .map_err(|e| format!("Failed to download VAD model: {}", e))?;

        if !response.status().is_success() {
            return Err(format!(
                "VAD model download failed with status: {}",
                response.status()
            ));
        }

        let bytes = response
            .bytes()
            .map_err(|e| format!("Failed to read VAD model bytes: {}", e))?;

        std::fs::write(&model_path, &bytes)
            .map_err(|e| format!("Failed to write VAD model: {}", e))?;

        log::info!(
            "VAD model downloaded ({} bytes) to {}",
            bytes.len(),
            model_path.display()
        );

        Ok(model_path)
    }
}
