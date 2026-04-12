//! Project Furo — Audio Capture (cpal)
//!
//! Thread-safe recorder that captures microphone input, resampling to 16 kHz
//! mono i16 and streaming chunks to a volume callback and a raw-chunk callback
//! for VAD.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
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
    /// - `profile`: Input profile with gain, noise gate, highpass, volume boost.
    /// - `on_volume`: Called with normalised volume (0.0–1.0) every ~50ms.
    /// - `on_raw_chunk`: Called with raw i16 PCM bytes for each chunk (for streaming VAD).
    pub fn start<FVol, FChunk>(
        &mut self,
        device_name: &str,
        profile: config::InputProfile,
        on_volume: FVol,
        on_raw_chunk: FChunk,
    ) -> Result<(), String>
    where
        FVol: Fn(f32) + Send + Sync + 'static,
        FChunk: Fn(&[i16]) + Send + Sync + 'static,
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

        // Negotiate a supported stream configuration.
        // Many macOS devices (e.g. MacBook Pro Microphone) only support their
        // native sample rate (typically 48 kHz). We accept whatever the device
        // offers and resample to 16 kHz mono in the callback.
        let (stream_config, device_rate, device_channels, sample_fmt) = Self::negotiate_config(&device)?;
        let need_resample = device_rate != config::AUDIO_RATE as u32;
        log::info!(
            "Audio config: {}Hz {}ch {:?} (need_resample={})",
            device_rate, device_channels, sample_fmt, need_resample
        );

        // Reset state
        self.frames.lock().clear();
        self.recording.store(true, Ordering::SeqCst);

        let frames = Arc::clone(&self.frames);
        let recording = Arc::clone(&self.recording);

        // Volume metering state (captured by closure)
        let last_volume_time = Arc::new(Mutex::new(Instant::now()));
        let noise_floor = Arc::new(Mutex::new(config::INITIAL_NOISE_FLOOR_DBFS));

        // Adaptive gate: counts chunks to drive the warm-up / calibration period.
        let chunk_count = Arc::new(Mutex::new(0u32));

        // Resampler state: accumulate fractional-sample position across callbacks
        let resample_pos = Arc::new(Mutex::new(0.0f64));
        let target_rate = config::AUDIO_RATE as f64;
        let source_rate = device_rate as f64;
        let ch = device_channels as usize;

        // Shared processing closure — takes already-converted i16 mono data
        // Wrapped in Arc so we can share it between potential match arms.
        let process_audio = Arc::new({
            let frames = Arc::clone(&frames);
            let recording = Arc::clone(&recording);
            let last_volume_time = Arc::clone(&last_volume_time);
            let noise_floor = Arc::clone(&noise_floor);
            let chunk_count = Arc::clone(&chunk_count);
            let resample_pos = Arc::clone(&resample_pos);

            // Input profile parameters
            let gain_linear = 10.0_f32.powf(profile.input_gain_db / 20.0);
            let noise_gate_linear = if profile.noise_gate_dbfs <= -59.0 {
                0.0_f32 // effectively disabled
            } else {
                32768.0 * 10.0_f32.powf(profile.noise_gate_dbfs / 20.0)
            };
            let vol_boost = profile.volume_display_boost;

            // Pre-VAD highpass filter state (1st-order IIR for efficiency in callback)
            let hp_cutoff = profile.pre_vad_highpass_hz;
            let hp_rc = 1.0 / (2.0 * std::f32::consts::PI * hp_cutoff);
            let hp_dt = 1.0 / config::AUDIO_RATE as f32;
            let hp_alpha = hp_rc / (hp_rc + hp_dt);
            let hp_prev_in = Arc::new(Mutex::new(0.0_f32));
            let hp_prev_out = Arc::new(Mutex::new(0.0_f32));

            move |mono_i16: Vec<i16>| {
                if !recording.load(Ordering::Relaxed) {
                    return;
                }

                let resampled = if need_resample {
                    resample_linear(&mono_i16, source_rate, target_rate, &resample_pos)
                } else {
                    mono_i16
                };

                if resampled.is_empty() {
                    return;
                }

                // Apply input profile processing: gain → highpass → noise gate
                let processed = {
                    let mut hp_in = hp_prev_in.lock();
                    let mut hp_out = hp_prev_out.lock();
                    resampled.iter().map(|&s| {
                        // 1) Apply gain
                        let gained = (s as f32 * gain_linear).clamp(-32767.0, 32767.0);

                        // 2) Highpass filter (1st-order IIR)
                        let filtered = hp_alpha * (*hp_out + gained - *hp_in);
                        *hp_in = gained;
                        *hp_out = filtered;

                        filtered.round() as i16
                    }).collect::<Vec<i16>>()
                };

                // 3) Adaptive noise gate.
                //
                // During the warm-up period (~256 ms) we only calibrate the
                // noise floor from the ambient room noise captured at the start
                // of each recording. After warm-up, the threshold is set to
                // max(profile_static_gate, noise_floor + ADAPTIVE_GATE_HEADROOM_DB)
                // so it automatically raises in noisy environments.
                let output = {
                    let sum_sq: f64 = processed.iter().map(|&s| (s as f64) * (s as f64)).sum();
                    let rms = (sum_sq / processed.len() as f64).sqrt() as f32;
                    let db = if rms > 1.0 {
                        20.0 * (rms / 32768.0_f32).log10()
                    } else {
                        -96.0_f32
                    };

                    // Update EMA noise floor every chunk (not just at volume events)
                    {
                        let mut nf = noise_floor.lock();
                        // Only pull floor down when signal is near current floor
                        // (don't let loud speech drag the floor upward).
                        if db < *nf + 3.0 {
                            *nf = *nf * (1.0 - config::NOISE_FLOOR_ALPHA)
                                + db * config::NOISE_FLOOR_ALPHA;
                        }
                    }

                    let mut count = chunk_count.lock();
                    *count += 1;
                    let warmed_up = *count > config::ADAPTIVE_GATE_WARM_UP_CHUNKS;
                    drop(count);

                    if warmed_up && (noise_gate_linear > 0.0 || true) {
                        let nf_db = *noise_floor.lock();
                        let adaptive_gate_db = nf_db + config::ADAPTIVE_GATE_HEADROOM_DB;
                        let adaptive_gate_linear =
                            32768.0_f32 * 10.0_f32.powf(adaptive_gate_db / 20.0);
                        // Use the tighter (higher) of adaptive vs. profile static gate
                        let effective_gate = adaptive_gate_linear.max(noise_gate_linear);
                        if rms < effective_gate {
                            vec![0i16; processed.len()]
                        } else {
                            processed
                        }
                    } else if noise_gate_linear > 0.0 && rms < noise_gate_linear {
                        // During warm-up, still apply the profile static gate as floor
                        vec![0i16; processed.len()]
                    } else {
                        processed
                    }
                };

                frames.lock().push_back(output.clone());
                on_raw_chunk(&output);

                // Throttled volume computation
                let now = Instant::now();
                let mut last = last_volume_time.lock();
                if now.duration_since(*last).as_millis()
                    >= config::VOLUME_THROTTLE_MS as u128
                {
                    *last = now;
                    let level = compute_volume(&output, &noise_floor);
                    // Apply volume display boost from profile
                    let boosted = (level * vol_boost).min(1.0);
                    on_volume(boosted);
                }
            }
        });

        // Build the stream with the device's native sample format
        let stream = match sample_fmt {
            SampleFormat::I16 => {
                let process = Arc::clone(&process_audio);
                device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        let mono: Vec<i16> = if ch > 1 {
                            data.chunks_exact(ch)
                                .map(|frame| {
                                    let sum: i32 = frame.iter().map(|&s| s as i32).sum();
                                    (sum / ch as i32) as i16
                                })
                                .collect()
                        } else {
                            data.to_vec()
                        };
                        process(mono);
                    },
                    |err| log::error!("Audio stream error: {}", err),
                    None,
                )
            }
            SampleFormat::F32 => {
                let process = Arc::clone(&process_audio);
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        let mono: Vec<i16> = if ch > 1 {
                            data.chunks_exact(ch)
                                .map(|frame| {
                                    let avg: f32 = frame.iter().sum::<f32>() / ch as f32;
                                    (avg.clamp(-1.0, 1.0) * 32767.0) as i16
                                })
                                .collect()
                        } else {
                            data.iter()
                                .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                                .collect()
                        };
                        process(mono);
                    },
                    |err| log::error!("Audio stream error: {}", err),
                    None,
                )
            }
            _ => {
                return Err(format!("Unsupported sample format: {:?}", sample_fmt));
            }
        }
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

    /// Negotiate a supported stream configuration for the device.
    /// Prefers 16 kHz mono but falls back to the device's default config
    /// (commonly 48 kHz on macOS) — we resample in the callback.
    fn negotiate_config(device: &Device) -> Result<(StreamConfig, u32, u16, SampleFormat), String> {
        let target_rate = config::AUDIO_RATE;

        // Try: exact 16 kHz mono i16 first
        if let Ok(configs) = device.supported_input_configs() {
            for cfg in configs {
                if cfg.channels() == 1
                    && cfg.min_sample_rate() <= target_rate
                    && cfg.max_sample_rate() >= target_rate
                    && cfg.sample_format() == SampleFormat::I16
                {
                    let sc = cfg.with_sample_rate(target_rate).config();
                    return Ok((sc, config::AUDIO_RATE, 1, SampleFormat::I16));
                }
            }
        }

        // Fallback: use the device's default input config
        let default_cfg = device
            .default_input_config()
            .map_err(|e| format!("Failed to get default input config: {}", e))?;

        let rate = default_cfg.sample_rate();
        let channels = default_cfg.channels();
        let fmt = default_cfg.sample_format();

        // Build a StreamConfig from the default
        let stream_config = StreamConfig {
            channels,
            sample_rate: default_cfg.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        log::info!(
            "Using device default config: {}Hz {}ch {:?} (will resample to {}Hz mono)",
            rate, channels, fmt, config::AUDIO_RATE
        );
        Ok((stream_config, rate, channels, fmt))
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

    // Noise floor is updated in the hot path (process_audio); just read it here.
    let nf = *noise_floor.lock();

    // Map between noise floor and ceiling
    let range = config::PEAK_CEILING_DBFS - nf;
    if range <= 0.0 {
        return 0.0;
    }

    let level = (db - nf) / range;
    level.clamp(0.0, 1.0)
}

/// Linear-interpolation resampler for real-time audio callback use.
///
/// Maintains fractional sample position across invocations via `pos` so that
/// the conversion is seamless across chunk boundaries.
fn resample_linear(
    input: &[i16],
    source_rate: f64,
    target_rate: f64,
    pos: &Arc<Mutex<f64>>,
) -> Vec<i16> {
    if input.is_empty() {
        return Vec::new();
    }

    let ratio = source_rate / target_rate;
    let mut p = pos.lock();
    let mut out = Vec::with_capacity((input.len() as f64 / ratio) as usize + 1);

    while (*p + 1.0) < input.len() as f64 {
        let idx = *p as usize;
        let frac = *p - idx as f64;
        let a = input[idx] as f64;
        let b = if idx + 1 < input.len() { input[idx + 1] as f64 } else { a };
        let sample = a + (b - a) * frac;
        out.push(sample.round() as i16);
        *p += ratio;
    }

    // Carry over the fractional position for the next callback
    *p -= input.len() as f64;
    if *p < 0.0 {
        *p = 0.0;
    }

    out
}
