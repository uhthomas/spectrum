use std::f32::consts::PI;

use rustfft::{FftPlanner, num_complex::Complex32};

const MAX_FFT_SIZE: usize = 32_768;

/// A native implementation of the frequency-domain algorithm specified for
/// Web Audio's `AnalyserNode`.
pub struct Analyser {
    samples: Vec<f32>,
    write: usize,
    available: usize,
    sample_rate: u32,
    smoothed: Vec<f32>,
    smoothing_fft_size: usize,
    planner: FftPlanner<f32>,
}

impl Default for Analyser {
    fn default() -> Self {
        Self {
            samples: vec![0.0; MAX_FFT_SIZE],
            write: 0,
            available: 0,
            sample_rate: 48_000,
            smoothed: Vec::new(),
            smoothing_fft_size: 0,
            planner: FftPlanner::new(),
        }
    }
}

impl Analyser {
    pub fn reset(&mut self) {
        self.samples.fill(0.0);
        self.write = 0;
        self.available = 0;
        self.smoothed.fill(0.0);
    }

    pub fn push_interleaved_f32le(&mut self, bytes: &[u8], channels: usize, sample_rate: u32) {
        if channels == 0 {
            return;
        }

        self.sample_rate = sample_rate;
        let frame_bytes = channels * size_of::<f32>();
        for frame in bytes.chunks_exact(frame_bytes) {
            let mut mono = 0.0;
            for channel in 0..channels {
                let offset = channel * size_of::<f32>();
                mono += f32::from_le_bytes(frame[offset..offset + 4].try_into().unwrap());
            }
            self.push(mono / channels as f32);
        }
    }

    pub fn push_silence(&mut self, frames: usize) {
        for _ in 0..frames {
            self.push(0.0);
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn push(&mut self, sample: f32) {
        self.samples[self.write] = sample;
        self.write = (self.write + 1) % self.samples.len();
        self.available = (self.available + 1).min(self.samples.len());
    }

    pub fn frequency_data(&mut self, bins: usize, smoothing: f32) -> Vec<f32> {
        let fft_size = fft_size_for_rate(self.sample_rate);
        let output_bins = bins.min(fft_size / 2);
        if self.available < fft_size || output_bins == 0 {
            return vec![f32::NEG_INFINITY; output_bins];
        }

        if self.smoothing_fft_size != fft_size {
            self.smoothed = vec![0.0; fft_size / 2];
            self.smoothing_fft_size = fft_size;
        }

        let oldest = (self.write + self.samples.len() - fft_size) % self.samples.len();
        let mut input = Vec::with_capacity(fft_size);
        for n in 0..fft_size {
            let sample = self.samples[(oldest + n) % self.samples.len()];
            let phase = 2.0 * PI * n as f32 / fft_size as f32;
            // Web Audio's Blackman window uses alpha=0.16 and N, not N-1.
            let window = 0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
            input.push(Complex32::new(sample * window, 0.0));
        }

        self.planner.plan_fft_forward(fft_size).process(&mut input);

        let normalizer = 1.0 / fft_size as f32;
        let smoothing = smoothing.clamp(0.0, 0.99);
        for (index, value) in input.iter().take(fft_size / 2).enumerate() {
            let magnitude = value.norm() * normalizer;
            self.smoothed[index] = smoothing * self.smoothed[index] + (1.0 - smoothing) * magnitude;
        }

        self.smoothed
            .iter()
            .take(output_bins)
            .map(|magnitude| 20.0 * magnitude.log10())
            .collect()
    }
}

pub fn fft_size_for_rate(sample_rate: u32) -> usize {
    match sample_rate {
        96_000 => 8 << 10,
        192_000 => 16 << 10,
        384_000 => 32 << 10,
        // This also preserves the source's 41000/48000 entries and fallback.
        _ => 4 << 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_browser_fft_size_mapping() {
        assert_eq!(fft_size_for_rate(41_000), 4096);
        assert_eq!(fft_size_for_rate(44_100), 4096);
        assert_eq!(fft_size_for_rate(48_000), 4096);
        assert_eq!(fft_size_for_rate(96_000), 8192);
        assert_eq!(fft_size_for_rate(192_000), 16384);
        assert_eq!(fft_size_for_rate(384_000), 32768);
    }

    #[test]
    fn silence_is_negative_infinity_after_warmup() {
        let mut analyser = Analyser::default();
        let silence = vec![0_u8; 4096 * 2 * 4];
        analyser.push_interleaved_f32le(&silence, 2, 48_000);
        assert!(
            analyser
                .frequency_data(4, 0.67)
                .iter()
                .all(|value| value.is_infinite())
        );
    }

    #[test]
    fn reset_discards_previous_samples() {
        let mut analyser = Analyser::default();
        let signal = vec![1_u8; 4096 * 2 * 4];
        analyser.push_interleaved_f32le(&signal, 2, 48_000);
        analyser.reset();
        assert!(
            analyser
                .frequency_data(4, 0.67)
                .iter()
                .all(|value| value.is_infinite())
        );
    }

    #[test]
    fn injected_silence_replaces_the_fft_window() {
        let mut analyser = Analyser::default();
        let mut signal = Vec::with_capacity(4096 * 2 * 4);
        for _ in 0..4096 * 2 {
            signal.extend_from_slice(&1.0_f32.to_le_bytes());
        }
        analyser.push_interleaved_f32le(&signal, 2, 48_000);
        analyser.push_silence(4096);
        assert!(
            analyser
                .frequency_data(4, 0.67)
                .iter()
                .all(|value| value.is_infinite())
        );
    }
}
