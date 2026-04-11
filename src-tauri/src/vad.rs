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
//!
//! On x86_64 macOS, ONNX Runtime prebuilt binaries are not available
//! (dropped as of 1.24.1). A passthrough stub is compiled instead —
//! all audio frames are treated as speech.

// ── Full ONNX-based VAD (all platforms except x86_64 macOS) ─────────────────
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod ort_impl {
    use ort::session::Session;
    use ort::value::Tensor;
    use parking_lot::Mutex;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::config;

    /// Streaming Silero VAD wrapper using ONNX Runtime.
    pub struct VoiceActivityDetector {
        session: Session,
        threshold: f32,
        sample_rate: i64,
        state: Arc<Mutex<Vec<f32>>>,
        context: Arc<Mutex<Vec<f32>>>,
    }

    impl VoiceActivityDetector {
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

        pub fn reset(&self) {
            self.state.lock().fill(0.0);
            self.context.lock().fill(0.0);
        }

        pub fn is_speech(&mut self, chunk_i16: &[i16]) -> bool {
            let chunk_f32: Vec<f32> = chunk_i16
                .iter()
                .map(|&s| s as f32 / 32768.0)
                .collect();

            let context = self.context.lock().clone();
            let mut input_data = Vec::with_capacity(context.len() + chunk_f32.len());
            input_data.extend_from_slice(&context);
            input_data.extend_from_slice(&chunk_f32);

            {
                let mut ctx = self.context.lock();
                let total = input_data.len();
                ctx.clear();
                ctx.extend_from_slice(&input_data[total.saturating_sub(64)..]);
            }

            let input_len = input_data.len();
            let sr_data = vec![self.sample_rate];
            let state_data = self.state.lock().clone();

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

            let speech_prob = match result["output"].try_extract_tensor::<f32>() {
                Ok((_, data)) => data[0],
                Err(e) => {
                    log::warn!("Failed to extract VAD output: {}", e);
                    return false;
                }
            };

            if let Ok((_, data)) = result["stateN"].try_extract_tensor::<f32>() {
                *self.state.lock() = data.to_vec();
            }

            speech_prob >= self.threshold
        }

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
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub use ort_impl::VoiceActivityDetector;

// ── Passthrough stub for x86_64 macOS (ONNX Runtime unavailable) ────────────
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
mod stub_impl {
    use std::path::PathBuf;

    /// Stub VAD that treats all audio as speech.
    /// Used on x86_64 macOS where ONNX Runtime prebuilt binaries are unavailable.
    pub struct VoiceActivityDetector;

    impl VoiceActivityDetector {
        pub fn new(_model_path: &PathBuf, _threshold: f32) -> Result<Self, String> {
            log::info!("VAD passthrough mode (ONNX Runtime unavailable on x86_64 macOS)");
            Ok(VoiceActivityDetector)
        }

        pub fn reset(&self) {}

        pub fn is_speech(&mut self, _chunk_i16: &[i16]) -> bool {
            true
        }

        pub fn ensure_model_downloaded(models_dir: &PathBuf) -> Result<PathBuf, String> {
            log::info!("Skipping VAD model download (x86_64 macOS passthrough mode)");
            Ok(models_dir.join("silero_vad.onnx"))
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub use stub_impl::VoiceActivityDetector;
