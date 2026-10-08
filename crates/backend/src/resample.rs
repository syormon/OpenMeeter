//! Drift-correcting resampler: plays a source at a rate a few ppm off 1.0 so a
//! sink on another clock neither starves nor piles up, without ever dropping or
//! repeating a sample.
//!
//! Interpolation is a 32-tap Kaiser-windowed sinc, so it is flat and alias-free
//! across the audible band; at a ratio of exactly 1 on a whole-sample position it
//! passes audio through unchanged.

use std::sync::OnceLock;

use crate::engine::CHANNELS;

/// Taps on each side of the interpolated point.
const HALF: usize = 16;
const TAPS: usize = 2 * HALF;
/// Fractional positions tabulated; values in between are linearly interpolated.
const PHASES: usize = 256;
/// Kaiser window shape: about 80 dB stopband.
const KAISER_BETA: f64 = 8.0;

/// `PHASES + 1` rows of `TAPS` coefficients; row `i` is for fractional position `i / PHASES`.
fn kernel() -> &'static [[f32; TAPS]] {
    static KERNEL: OnceLock<Vec<[f32; TAPS]>> = OnceLock::new();
    KERNEL.get_or_init(|| {
        (0..=PHASES)
            .map(|phase| {
                let frac = phase as f64 / PHASES as f64;
                let mut row = [0f64; TAPS];
                for (m, c) in row.iter_mut().enumerate() {
                    // Distance from the interpolated point to input sample m.
                    let t = frac + (HALF - 1) as f64 - m as f64;
                    *c = sinc(t) * kaiser(t / HALF as f64);
                }
                let sum: f64 = row.iter().sum();
                row.map(|c| (c / sum) as f32)
            })
            .collect()
    })
}

fn sinc(t: f64) -> f64 {
    if t.fract() == 0.0 {
        return if t == 0.0 { 1.0 } else { 0.0 };
    }
    let x = std::f64::consts::PI * t;
    x.sin() / x
}

fn kaiser(x: f64) -> f64 {
    if x.abs() >= 1.0 {
        return 0.0;
    }
    bessel_i0(KAISER_BETA * (1.0 - x * x).sqrt()) / bessel_i0(KAISER_BETA)
}

/// Modified Bessel function of the first kind, order 0 (power series).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, q) = (1.0, 1.0, x * x / 4.0);
    for k in 1..50 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// One source's stream through the resampler. Input frames are appended as
/// needed; each output frame interpolates around a fractional read position that
/// advances by `ratio` input frames per output frame.
pub(crate) struct DriftResampler {
    /// Interleaved input frames still needed for interpolation.
    hist: Vec<f32>,
    /// Read position in frames within `hist`.
    pos: f64,
}

impl DriftResampler {
    /// Input frames needed beyond a block's length to interpolate it. One more
    /// than the kernel reaches: the read position is accumulated frame by frame,
    /// and rounding may carry it one frame past where [`Self::needed`] predicted.
    pub const LOOKAHEAD: usize = HALF + 2;

    pub fn new() -> Self {
        let mut r = Self { hist: Vec::new(), pos: 0.0 };
        r.reset();
        r
    }

    /// Start over from silence, e.g. after an underrun.
    pub fn reset(&mut self) {
        // Leading silence lets the first input frame sit exactly on the read position.
        self.hist.clear();
        self.hist.resize((HALF - 1) * CHANNELS, 0.0);
        self.pos = (HALF - 1) as f64;
        kernel();
    }

    /// Make room for blocks of up to `frames` output frames without allocating.
    pub fn reserve(&mut self, frames: usize) {
        let want = (frames + frames / 64 + TAPS + 4) * CHANNELS;
        self.hist.reserve(want.saturating_sub(self.hist.len()));
    }

    fn frames(&self) -> usize {
        self.hist.len() / CHANNELS
    }

    /// Input frames held that haven't been played yet (can be slightly negative).
    pub fn buffered(&self) -> f64 {
        self.frames() as f64 - self.pos - HALF as f64
    }

    /// More input frames needed to produce `frames` output frames at `ratio`.
    pub fn needed(&self, frames: usize, ratio: f64) -> usize {
        if frames == 0 {
            return 0;
        }
        let last = self.pos + (frames - 1) as f64 * ratio;
        (last as usize + Self::LOOKAHEAD).saturating_sub(self.frames())
    }

    /// Space for `frames` more input frames at the end; truncate with [`Self::commit`].
    pub fn input_slot(&mut self, frames: usize) -> &mut [f32] {
        let start = self.hist.len();
        self.hist.resize(start + frames * CHANNELS, 0.0);
        &mut self.hist[start..]
    }

    /// Keep only `samples` of the slot last handed out by [`Self::input_slot`].
    pub fn commit(&mut self, slot_frames: usize, samples: usize) {
        let len = self.hist.len() - slot_frames * CHANNELS + samples;
        self.hist.truncate(len);
    }

    /// Write `out.len() / CHANNELS` frames. Needs [`Self::needed`] more input first.
    pub fn process(&mut self, out: &mut [f32], ratio: f64) {
        let table = kernel();
        for frame in out.as_chunks_mut::<CHANNELS>().0 {
            let base = self.pos as usize;
            let frac = (self.pos - base as f64) * PHASES as f64;
            let phase = (frac as usize).min(PHASES - 1);
            let mix = (frac - phase as f64) as f32;
            let (a, b) = (&table[phase], &table[phase + 1]);
            let start = (base + 1 - HALF) * CHANNELS;
            let window = &self.hist[start..start + TAPS * CHANNELS];
            let mut acc = [0f32; CHANNELS];
            for (m, input) in window.as_chunks::<CHANNELS>().0.iter().enumerate() {
                let c = a[m] + (b[m] - a[m]) * mix;
                for (acc, x) in acc.iter_mut().zip(input) {
                    *acc += c * x;
                }
            }
            *frame = acc;
            self.pos += ratio;
        }
        // Drop input that no future output frame can reach.
        let done = (self.pos as usize + 1).saturating_sub(HALF);
        if done > 0 {
            self.hist.drain(..done * CHANNELS);
            self.pos -= done as f64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `input` through at a fixed ratio in blocks of `block` output frames.
    fn run(input: &[f32], ratio: f64, block: usize, blocks: usize) -> Vec<f32> {
        let mut r = DriftResampler::new();
        let mut fed = 0;
        let mut out = Vec::new();
        for _ in 0..blocks {
            let need = r.needed(block, ratio);
            let slot = r.input_slot(need);
            slot.copy_from_slice(&input[fed * CHANNELS..(fed + need) * CHANNELS]);
            fed += need;
            let mut buf = vec![0f32; block * CHANNELS];
            r.process(&mut buf, ratio);
            out.extend_from_slice(&buf);
        }
        out
    }

    #[test]
    fn unity_ratio_is_transparent() {
        let input: Vec<f32> = (0..4000).map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5).collect();
        let out = run(&input, 1.0, 480, 3);
        for (i, (o, x)) in out.iter().zip(&input).enumerate() {
            assert!((o - x).abs() < 1e-6, "sample {i}: {o} vs {x}");
        }
    }

    #[test]
    fn drifting_ratio_keeps_a_high_tone_clean() {
        // 15 kHz played 500 ppm fast must come out as a pure 15 kHz * 1.0005 tone.
        let (f, ratio) = (15_000.0 / 48_000.0, 1.0005);
        let input: Vec<f32> = (0..30_000).flat_map(|i| {
            let x = (std::f64::consts::TAU * f * i as f64).sin() as f32 * 0.5;
            [x, -x]
        }).collect();
        let out = run(&input, ratio, 441, 50);
        // Output frame n sits at input position (HALF - 1) - (HALF - 1) + n * ratio = n * ratio.
        let mut err = 0f64;
        let mut sig = 0f64;
        for (n, frame) in out.as_chunks::<CHANNELS>().0.iter().enumerate().skip(64) {
            let ideal = (std::f64::consts::TAU * f * n as f64 * ratio).sin() * 0.5;
            err += (frame[0] as f64 - ideal).powi(2) + (frame[1] as f64 + ideal).powi(2);
            sig += 2.0 * ideal * ideal;
        }
        let snr_db = 10.0 * (sig / err).log10();
        assert!(snr_db > 70.0, "SNR {snr_db:.1} dB");
    }

    #[test]
    fn position_stays_bounded() {
        let input = vec![0.1f32; 200_000 * CHANNELS];
        let mut r = DriftResampler::new();
        let mut fed = 0;
        for block in [480usize, 17, 1024, 1, 480].into_iter().cycle().take(200) {
            let need = r.needed(block, 0.999);
            r.input_slot(need).copy_from_slice(&input[fed * CHANNELS..(fed + need) * CHANNELS]);
            fed += need;
            let mut out = vec![0f32; block * CHANNELS];
            r.process(&mut out, 0.999);
            assert!(r.frames() < TAPS + 4 && r.buffered() < 3.0 && r.buffered() > -2.0 - HALF as f64);
        }
    }
}
