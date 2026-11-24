//! Look-ahead brickwall peak limiter, always stereo-linked.
//!
//! Gain path: required gain `ceiling / |x|` -> sliding minimum
//! over the look-ahead window -> release smoothing (instant down, one-pole
//! up) -> moving average over the look-ahead window. The minimum-hold plus
//! moving average of equal length `L` guarantees that the gain applied to a
//! sample delayed by `L - 1` frames is at most the gain that sample
//! requires, so the ceiling is never exceeded and the attack is a smooth
//! linear ramp instead of a click.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use crate::util::{db_to_lin, lin_to_db, peak, time_coef, Smoother};
use crate::{Filter, FilterDesc, GraphKind, ParamInfo};

const INPUT_GAIN: usize = 0;
const CEILING: usize = 1;
const RELEASE: usize = 2;
const LOOKAHEAD: usize = 3;

const MAX_LOOKAHEAD_MS: f32 = 20.0;

const PARAMS: &[ParamInfo] = &[
    ParamInfo::float("input-gain", "Input gain", "dB", 0.0, 24.0, 0.0),
    ParamInfo::float("ceiling", "Ceiling", "dB", -24.0, 0.0, -1.0),
    ParamInfo::float("release", "Release", "ms", 1.0, 1000.0, 50.0),
    ParamInfo::float("lookahead", "Look-ahead", "ms", 0.0, MAX_LOOKAHEAD_MS, 5.0),
];

pub static DESC: FilterDesc = FilterDesc {
    name: "limiter",
    label: "Limiter",
    description: "Look-ahead brickwall peak limiter",
    params: PARAMS,
    graph: GraphKind::Transfer,
    meter: Some(("gain-reduction", "dB")),
    new: |fs, ch| Box::new(Limiter::new(fs as f64, ch)),
};

/// Gain path state.
struct Detector {
    /// Monotonic deque of (frame index, required gain) for the sliding min.
    hold: VecDeque<(u64, f64)>,
    release: f64,
    ring: Vec<f64>,
    sum: f64,
}

impl Detector {
    fn new(capacity: usize) -> Self {
        Self {
            hold: VecDeque::with_capacity(capacity + 1),
            release: 1.0,
            ring: vec![1.0; capacity],
            sum: 0.0,
        }
    }

    fn reset(&mut self, len: usize) {
        self.hold.clear();
        self.release = 1.0;
        self.ring[..len].fill(1.0);
        self.sum = len as f64;
    }

    /// Feed the required gain of frame `n`, return the gain to apply to the
    /// frame delayed by `len - 1`.
    #[inline]
    fn tick(&mut self, n: u64, required: f64, len: usize, release_coef: f64) -> f64 {
        while matches!(self.hold.back(), Some(&(_, v)) if v >= required) {
            self.hold.pop_back();
        }
        self.hold.push_back((n, required));
        while matches!(self.hold.front(), Some(&(i, _)) if i + (len as u64) <= n) {
            self.hold.pop_front();
        }
        let held = self.hold.front().map_or(required, |&(_, v)| v);

        self.release = if held < self.release {
            held
        } else {
            held + (self.release - held) * release_coef
        };

        let pos = (n % len as u64) as usize;
        self.sum += self.release - self.ring[pos];
        self.ring[pos] = self.release;
        if pos == len - 1 {
            // Recompute regularly so rounding errors cannot accumulate.
            self.sum = self.ring[..len].iter().sum();
        }
        self.sum / len as f64
    }
}

/// Swap `x` into the delay line (of `len - 1` frames) and return the
/// sample it replaces.
#[inline]
fn delay_swap(delay: &mut [f64], len: usize, index: usize, x: f64) -> f64 {
    if len > 1 {
        core::mem::replace(&mut delay[index], x)
    } else {
        x
    }
}

pub struct Limiter {
    fs: f64,
    channels: usize,
    /// Linear gains, smoothed.
    input_gain: Smoother,
    ceiling: Smoother,
    release_coef: f64,
    /// Look-ahead window length in frames (>= 1); latency is `len - 1`.
    len: usize,
    detector: Detector,
    delay: Vec<f64>,
    /// Ceiling each delayed frame's gain was computed for.
    ceilings: Vec<f64>,
    frame: u64,
    meter: f64,
}

impl Limiter {
    fn new(fs: f64, channels: usize) -> Self {
        let capacity = Self::window_len(MAX_LOOKAHEAD_MS as f64, fs);
        Self {
            fs,
            channels,
            input_gain: Smoother::new(1.0, fs),
            ceiling: Smoother::new(1.0, fs),
            release_coef: 0.0,
            len: 1,
            detector: Detector::new(capacity),
            delay: vec![0.0; capacity * channels],
            ceilings: vec![0.0; capacity],
            frame: 0,
            meter: 0.0,
        }
    }

    fn window_len(ms: f64, fs: f64) -> usize {
        (libm::round(ms * fs / 1000.0) as usize).max(1)
    }
}

impl Filter for Limiter {
    fn set_param(&mut self, index: usize, value: f32) {
        let Some(info) = PARAMS.get(index) else { return };
        let v = info.sanitize(value) as f64;
        match index {
            INPUT_GAIN => self.input_gain.set(db_to_lin(v)),
            CEILING => self.ceiling.set(db_to_lin(v)),
            RELEASE => self.release_coef = time_coef(v, self.fs),
            LOOKAHEAD => {
                let len = Self::window_len(v, self.fs);
                if len != self.len {
                    // Changing the delay cannot be done seamlessly; restart.
                    self.len = len;
                    self.reset();
                }
            }
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let ch = self.channels;
        let len = self.len;
        let mut meter = 1.0f64;
        for frame in buf.chunks_exact_mut(ch) {
            // NaN/Inf would get through the clamp below.
            for s in frame.iter_mut().filter(|s| !s.is_finite()) {
                *s = 0.0;
            }
            let input_gain = self.input_gain.next();
            let ceiling = self.ceiling.next();
            let n = self.frame;
            let slot = if len > 1 { (n % (len as u64 - 1)) as usize * ch } else { 0 };

            let peak = peak(frame) * input_gain;
            let required = if peak > ceiling { ceiling / peak } else { 1.0 };
            let g = self.detector.tick(n, required, len, self.release_coef);
            if g < meter {
                meter = g;
            }
            let ceiling = delay_swap(&mut self.ceilings, len, slot / ch, ceiling);
            for (c, s) in frame.iter_mut().enumerate() {
                let delayed = delay_swap(&mut self.delay, len, slot + c, *s as f64 * input_gain);
                *s = (delayed * g).clamp(-ceiling, ceiling) as f32;
            }
            self.frame += 1;
        }
        self.meter = lin_to_db(meter);
    }

    fn reset(&mut self) {
        self.delay.fill(0.0);
        self.ceilings.fill(self.ceiling.target());
        self.detector.reset(self.len);
        self.frame = 0;
        self.meter = 0.0;
        self.input_gain.snap();
        self.ceiling.snap();
    }

    fn latency(&self) -> usize {
        self.len - 1
    }

    fn meter(&self) -> f32 {
        self.meter as f32
    }

    fn graph(&self, x: f32) -> f32 {
        let y = x as f64 + lin_to_db(self.input_gain.target());
        y.min(lin_to_db(self.ceiling.target())) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_exceeds_ceiling() {
        let mut l = crate::create(&DESC, 48000.0, 2);
        l.set_param(INPUT_GAIN, 12.0);
        l.set_param(CEILING, -1.0);
        l.reset();
        let ceiling = db_to_lin(-1.0) as f32;
        // Deterministic pseudo-random bursts.
        let mut seed = 1u32;
        let mut buf: Vec<f32> = (0..96000)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0;
                if (i / 4800) % 2 == 0 { noise } else { noise * 0.05 }
            })
            .collect();
        for chunk in buf.chunks_mut(256) {
            l.process(chunk);
        }
        let max = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(max <= ceiling, "{max} > {ceiling}");
        assert!(max > ceiling * 0.9);
    }

    #[test]
    fn latency_matches_delay() {
        let mut l = crate::create(&DESC, 48000.0, 1);
        let lat = l.latency();
        let mut buf = vec![0.0f32; 1000];
        buf[0] = 0.1;
        l.process(&mut buf);
        assert_eq!(buf.iter().position(|&s| s != 0.0), Some(lat));
    }
}
