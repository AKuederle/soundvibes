use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

use crate::{Error, Result};

/// Time-major, normalized 128-band features for the native encoder.
pub struct Features {
    pub frames: usize,
    pub values: Vec<f32>,
}

/// The official CPU front end: 16 kHz mono PCM, centered constant padding,
/// symmetric Hann window, Slaney filters, and sample-standard-deviation normalization.
pub fn log_mel(samples: &[f32]) -> Result<Features> {
    if samples.len() < 160 || samples.len() > 35 * 16_000 || samples.iter().any(|v| !v.is_finite())
    {
        return Err(Error::new(
            "log-mel requires 10 ms to 35 seconds of finite 16 kHz mono audio",
        ));
    }
    let frames = samples.len() / 160 + 1;
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(512);
    let mut scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
    let mut spectrum = vec![Complex::default(); 512];
    let window: Vec<f32> = (0..400)
        .map(|i| 0.5 - 0.5 * (i as f32 * (2.0 * std::f64::consts::PI / 399.0) as f32).cos())
        .collect();
    let mut wave = samples.to_vec();
    for i in 1..wave.len() {
        wave[i] = samples[i] - 0.97 * samples[i - 1];
    }
    let filters = mel_filters();
    let mut values = vec![0.0; frames * 128];
    for frame in 0..frames {
        spectrum.fill(Complex::default());
        for i in 0..400 {
            let index = (frame * 160 + 56 + i) as isize - 256;
            if index >= 0 && (index as usize) < wave.len() {
                spectrum[56 + i].re = wave[index as usize] * window[i];
            }
        }
        fft.process_with_scratch(&mut spectrum, &mut scratch);
        let mut power = [0.0; 257];
        for (p, s) in power.iter_mut().zip(&spectrum) {
            *p = s.norm_sqr();
        }
        for mel in 0..128 {
            let energy: f32 = filters[mel * 257..(mel + 1) * 257]
                .iter()
                .zip(power)
                .map(|(w, p)| w * p)
                .sum();
            values[frame * 128 + mel] = (energy + 2.0f32.powi(-24)).ln();
        }
    }
    for mel in 0..128 {
        let mean = (0..frames)
            .map(|t| values[t * 128 + mel] as f64)
            .sum::<f64>()
            / frames as f64;
        let variance = (0..frames)
            .map(|t| (values[t * 128 + mel] as f64 - mean).powi(2))
            .sum::<f64>()
            / (frames - 1) as f64;
        let denominator = variance.sqrt() + 1e-5;
        for t in 0..frames {
            values[t * 128 + mel] = ((values[t * 128 + mel] as f64 - mean) / denominator) as f32;
        }
    }
    Ok(Features { frames, values })
}

fn mel_filters() -> Vec<f32> {
    let hz_to_mel = |f: f64| {
        if f < 1000.0 {
            f / (200.0 / 3.0)
        } else {
            15.0 + (f / 1000.0).ln() / (6.4f64.ln() / 27.0)
        }
    };
    let mel_to_hz = |m: f64| {
        if m < 15.0 {
            m * (200.0 / 3.0)
        } else {
            1000.0 * ((m - 15.0) * (6.4f64.ln() / 27.0)).exp()
        }
    };
    let max_mel = hz_to_mel(8000.0);
    let edges: Vec<f64> = (0..130)
        .map(|i| mel_to_hz(i as f64 * max_mel / 129.0))
        .collect();
    let mut filters = vec![0.0; 128 * 257];
    for mel in 0..128 {
        for bin in 0..257 {
            let hz = bin as f64 * 16000.0 / 512.0;
            let lower = (hz - edges[mel]) / (edges[mel + 1] - edges[mel]);
            let upper = (edges[mel + 2] - hz) / (edges[mel + 2] - edges[mel + 1]);
            filters[mel * 257 + bin] =
                (lower.min(upper).max(0.0) * 2.0 / (edges[mel + 2] - edges[mel])) as f32;
        }
    }
    filters
}
