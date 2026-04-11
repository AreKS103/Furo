//! Project Furo — DSP Conditioning Chain
//!
//! Biquad-based processing: highpass → high-shelf → compressor.
//! All processing operates on f32 samples in [-1.0, 1.0] at 16 kHz.

use crate::config;

/// Second-order biquad filter (Direct Form I).
struct BiquadFilter {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BiquadFilter {
    /// 2nd-order Butterworth highpass filter.
    fn highpass(cutoff_hz: f32, sample_rate: f32) -> Self {
        let omega = 2.0 * std::f32::consts::PI * cutoff_hz / sample_rate;
        let cos_w = omega.cos();
        let sin_w = omega.sin();
        let alpha = sin_w / (2.0 * std::f32::consts::FRAC_1_SQRT_2); // Q = 1/sqrt(2)

        let b0 = (1.0 + cos_w) / 2.0;
        let b1 = -(1.0 + cos_w);
        let b2 = (1.0 + cos_w) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w;
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// High-shelf filter (peaking EQ shelf).
    fn high_shelf(cutoff_hz: f32, gain_db: f32, sample_rate: f32) -> Self {
        let a = 10.0_f32.powf(gain_db / 40.0); // sqrt of linear gain
        let omega = 2.0 * std::f32::consts::PI * cutoff_hz / sample_rate;
        let cos_w = omega.cos();
        let sin_w = omega.sin();
        let alpha = sin_w / 2.0 * ((a + 1.0 / a) * (1.0 / 0.7071 - 1.0) + 2.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    #[inline]
    fn process_sample(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

/// Simple envelope-follower compressor (feed-forward, no look-ahead).
struct Compressor {
    threshold_linear: f32,
    ratio: f32,
    envelope: f32,
    attack_coeff: f32,
    release_coeff: f32,
}

impl Compressor {
    fn new(threshold_db: f32, ratio: f32, sample_rate: f32) -> Self {
        let threshold_linear = 10.0_f32.powf(threshold_db / 20.0);
        // Attack ~5ms, Release ~50ms
        let attack_coeff = (-1.0 / (0.005 * sample_rate)).exp();
        let release_coeff = (-1.0 / (0.050 * sample_rate)).exp();
        Self {
            threshold_linear,
            ratio,
            envelope: 0.0,
            attack_coeff,
            release_coeff,
        }
    }

    #[inline]
    fn process_sample(&mut self, x: f32) -> f32 {
        let abs_x = x.abs();

        // Envelope follower
        if abs_x > self.envelope {
            self.envelope = self.attack_coeff * self.envelope + (1.0 - self.attack_coeff) * abs_x;
        } else {
            self.envelope =
                self.release_coeff * self.envelope + (1.0 - self.release_coeff) * abs_x;
        }

        // Gain computation
        if self.envelope > self.threshold_linear {
            let over_db = 20.0 * (self.envelope / self.threshold_linear).log10();
            let compressed_db = over_db / self.ratio;
            let gain_reduction_db = over_db - compressed_db;
            let gain = 10.0_f32.powf(-gain_reduction_db / 20.0);
            x * gain
        } else {
            x
        }
    }

    fn reset(&mut self) {
        self.envelope = 0.0;
    }
}

/// Complete DSP chain: HighpassFilter(300 Hz) → HighShelfFilter(2500 Hz, +5 dB) → Compressor(-20 dB, 3:1)
pub struct DspChain {
    highpass: BiquadFilter,
    high_shelf: BiquadFilter,
    compressor: Compressor,
}

impl DspChain {
    pub fn new() -> Self {
        let sr = config::AUDIO_RATE as f32;
        Self {
            highpass: BiquadFilter::highpass(config::DSP_HIGHPASS_CUTOFF, sr),
            high_shelf: BiquadFilter::high_shelf(
                config::DSP_HIGH_SHELF_CUTOFF,
                config::DSP_HIGH_SHELF_GAIN_DB,
                sr,
            ),
            compressor: Compressor::new(
                config::DSP_COMPRESSOR_THRESHOLD_DB,
                config::DSP_COMPRESSOR_RATIO,
                sr,
            ),
        }
    }

    /// Process audio samples in-place. Samples must be f32 in [-1.0, 1.0].
    pub fn process(&mut self, audio: &mut [f32]) {
        for sample in audio.iter_mut() {
            *sample = self.highpass.process_sample(*sample);
            *sample = self.high_shelf.process_sample(*sample);
            *sample = self.compressor.process_sample(*sample);
        }
    }

    /// Reset all filter states. Call between recording sessions.
    pub fn reset(&mut self) {
        self.highpass.reset();
        self.high_shelf.reset();
        self.compressor.reset();
    }
}
