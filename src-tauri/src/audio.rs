//! Project Furo — Audio Capture (cpal)
//!
//! Thread-safe recorder that captures microphone input at 16 kHz mono i16,
//! streaming chunks to a volume callback and a raw-chunk callback for VAD.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::config;

/// Information about a microphone device.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MicInfo {
    pub name: String,
    pub index: usize,
}

/// Audio recorder using cpal. Captures raw PCM i16 at 16 kHz mono.
pub struct AudioRecorder {
    stream: Option<Stream>,
    frames: Arc<Mutex<VecDeque<Vec<i16>>>>,
    recording: Arc<AtomicBool>,
}

// SAFETY: The cpal::Stream handle contains a *mut c_void (WASAPI COM pointer)
// that is not Send by default, but the stream is only accessed via our
// AudioRecorder methods while holding the enclosing parking_lot::Mutex.
// The stream is created and dropped on the same logical thread context.
unsafe impl Send for AudioRecorder {}

impl AudioRecorder {
    pub fn new() -> Self {
        Self {
            stream: None,
            frames: Arc::new(Mutex::new(VecDeque::new())),
            recording: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Begin capturing audio from the specified device (or system default).
    ///
    /// - `device_name`: If non-empty, selects a specific input device by name.
    /// - `on_volume`: Called with normalised volume (0.0–1.0) every ~50ms.
    /// - `on_raw_chunk`: Called with raw i16 PCM bytes for each chunk (for streaming VAD).
    pub fn start<FVol, FChunk>(
        &mut self,
        device_name: &str,
        on_volume: FVol,
        on_raw_chunk: FChunk,
    ) -> Result<(), String>
    where
        FVol: Fn(f32) + Send + 'static,
        FChunk: Fn(&[i16]) + Send + 'static,
    {
        let host = cpal::default_host();

        // Select device
        let device = if device_name.is_empty() {
            host.default_input_device()
                .ok_or_else(|| "No default input device found".to_string())?
        } else {
            Self::find_device_by_name(&host, device_name)?
        };

        let device_display = device.description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "unknown".into());
        log::info!("Opening audio device: {}", device_display);

        let desired_config = StreamConfig {
            channels: config::AUDIO_CHANNELS,
            sample_rate: config::AUDIO_RATE,
            buffer_size: cpal::BufferSize::Fixed(config::AUDIO_CHUNK),
        };

        // Reset state
        self.frames.lock().clear();
        self.recording.store(true, Ordering::SeqCst);

        let frames = Arc::clone(&self.frames);
        let recording = Arc::clone(&self.recording);

        // Volume metering state (captured by closure)
        let last_volume_time = Arc::new(Mutex::new(Instant::now()));
        let noise_floor = Arc::new(Mutex::new(config::INITIAL_NOISE_FLOOR_DBFS));

        let stream = device
            .build_input_stream(
                &desired_config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    if !recording.load(Ordering::Relaxed) {
                        return;
                    }

                    let chunk = data.to_vec();
                    frames.lock().push_back(chunk);
                    on_raw_chunk(data);

                    // Throttled volume computation
                    let now = Instant::now();
                    let mut last = last_volume_time.lock();
                    if now.duration_since(*last).as_millis()
                        >= config::VOLUME_THROTTLE_MS as u128
                    {
                        *last = now;
                        let level = compute_volume(data, &noise_floor);
                        on_volume(level);
                    }
                },
                move |err| {
                    log::error!("Audio stream error: {}", err);
                },
                None, // timeout
            )
            .map_err(|e| format!("Failed to build input stream: {}", e))?;

        stream
            .play()
            .map_err(|e| format!("Failed to start audio stream: {}", e))?;

        self.stream = Some(stream);
        log::debug!("Recording started (streaming mode).");
        Ok(())
    }

    /// Stop recording and return all captured audio as concatenated i16 samples.
    pub fn stop(&mut self) -> Vec<i16> {
        self.recording.store(false, Ordering::SeqCst);

        // Drop the stream to release the audio device.
        self.stream = None;

        let mut frames = self.frames.lock();
        let total_samples: usize = frames.iter().map(|f| f.len()).sum();
        let mut all = Vec::with_capacity(total_samples);
        for frame in frames.drain(..) {
            all.extend_from_slice(&frame);
        }

        let duration_s = all.len() as f32 / (config::AUDIO_RATE as f32);
        log::debug!("Recording stopped. Captured {:.2}s of audio.", duration_s);
        all
    }

    /// Check if recording is active.
    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Relaxed)
    }

    /// List available input devices, filtered by exclude keywords.
    pub fn list_devices() -> Vec<MicInfo> {
        let host = cpal::default_host();
        let devices = match host.input_devices() {
            Ok(d) => d,
            Err(e) => {
                log::warn!("Failed to enumerate input devices: {}", e);
                return Vec::new();
            }
        };

        let mut mics = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        for (idx, device) in devices.enumerate() {
            let name = match device.description() {
                Ok(d) => d.name().to_string(),
                Err(_) => continue,
            };

            // Check supported configs — skip devices that can't do input
            if device.supported_input_configs().is_err() {
                continue;
            }

            let name_lower = name.to_lowercase();

            if config::MIC_EXCLUDE_KEYWORDS
                .iter()
                .any(|kw| name_lower.contains(kw))
            {
                continue;
            }

            if seen_names.contains(&name) {
                continue;
            }
            seen_names.insert(name.clone());

            mics.push(MicInfo { name, index: idx });
        }

        mics
    }

    fn find_device_by_name(host: &cpal::Host, name: &str) -> Result<Device, String> {
        let devices = host
            .input_devices()
            .map_err(|e| format!("Failed to enumerate devices: {}", e))?;

        for device in devices {
            if let Ok(desc) = device.description() {
                if desc.name() == name {
                    return Ok(device);
                }
            }
        }

        // Fallback to default
        log::warn!(
            "Device '{}' not found, falling back to system default.",
            name
        );
        host.default_input_device()
            .ok_or_else(|| "No default input device available".into())
    }
}

/// Compute normalised volume (0.0–1.0) using dBFS with EMA noise floor tracking.
fn compute_volume(data: &[i16], noise_floor: &Arc<Mutex<f32>>) -> f32 {
    if data.is_empty() {
        return 0.0;
    }

    // RMS in float domain
    let sum_sq: f64 = data.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum_sq / data.len() as f64).sqrt() as f32;

    if rms < 1.0 {
        return 0.0;
    }

    // Convert to dBFS (reference = 32768)
    let db = 20.0 * (rms / 32768.0).log10();

    // Update EMA noise floor
    let mut nf = noise_floor.lock();
    if db < *nf + 3.0 {
        *nf = *nf * (1.0 - config::NOISE_FLOOR_ALPHA) + db * config::NOISE_FLOOR_ALPHA;
    }

    // Map between noise floor and ceiling
    let range = config::PEAK_CEILING_DBFS - *nf;
    if range <= 0.0 {
        return 0.0;
    }

    let level = (db - *nf) / range;
    level.clamp(0.0, 1.0)
}
