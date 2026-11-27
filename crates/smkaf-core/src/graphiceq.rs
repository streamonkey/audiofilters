//! 20-band graphic equalizer: peaking filters at fixed half-octave centre
//! frequencies (22 Hz to 16 kHz), one gain slider each.
//!
//! Neighbouring bands overlap, so setting each filter to its slider value
//! would make the curve miss the sliders (two adjacent +6 dB sliders give
//! about +9 dB in between). Instead the filter gains are solved so that the
//! combined response passes through every slider at its centre frequency
//! (Välimäki & Liski, "Accurate Cascade Graphic Equalizer", 2017). The solve
//! runs once after slider changes, however many sliders changed; the solved
//! gains are smoothed at the control rate like the parametric EQ's
//! parameters.
//!
//! Bands above `MAX_BAND_RATIO` of the sample rate are inactive (their
//! sliders have no effect), e.g. 16 kHz at 32 kHz.

use core::cell::Cell;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use crate::biquad::{Coefs, Probe, Shape, State};
use crate::util::{db_to_lin, lin_to_db, Smoother, CONTROL_FRAMES};
use crate::{Filter, FilterDesc, GraphKind, ParamInfo};

const BANDS: usize = 20;
const OUTPUT: usize = BANDS;

/// Slider range in dB.
const RANGE: f32 = 12.0;
/// Limit for the solved filter gains. Extreme slider patterns (alternating
/// ±12 dB) would otherwise need ever larger opposing gains.
const MAX_FILTER_GAIN: f64 = 24.0;
/// Q of the band filters. Lower Q (wider bands) gives less ripple between
/// the centres but more overlap, so extreme slider patterns hit
/// `MAX_FILTER_GAIN`. 2.3 keeps the ripple of a flat +12 dB setting near
/// 1 dB and still reaches alternating ±12 dB sliders exactly.
const Q: f64 = 2.3;
/// Bands at or above this fraction of the sample rate are inactive.
const MAX_BAND_RATIO: f64 = 0.4;
/// Re-linearization rounds of the gain solve, see `GraphicEq::solve`.
const SOLVE_ROUNDS: usize = 3;

macro_rules! band {
    ($name:literal, $label:literal) => {
        ParamInfo::float($name, $label, "dB", -RANGE, RANGE, 0.0)
    };
}

const PARAMS: &[ParamInfo; BANDS + 1] = &[
    band!("band-22", "22 Hz"),
    band!("band-31", "31 Hz"),
    band!("band-44", "44 Hz"),
    band!("band-63", "63 Hz"),
    band!("band-88", "88 Hz"),
    band!("band-125", "125 Hz"),
    band!("band-177", "177 Hz"),
    band!("band-250", "250 Hz"),
    band!("band-354", "354 Hz"),
    band!("band-500", "500 Hz"),
    band!("band-707", "707 Hz"),
    band!("band-1k", "1 kHz"),
    band!("band-1k4", "1.4 kHz"),
    band!("band-2k", "2 kHz"),
    band!("band-2k8", "2.8 kHz"),
    band!("band-4k", "4 kHz"),
    band!("band-5k7", "5.7 kHz"),
    band!("band-8k", "8 kHz"),
    band!("band-11k", "11 kHz"),
    band!("band-16k", "16 kHz"),
    ParamInfo::float("output", "Output gain", "dB", -24.0, 24.0, 0.0),
];

pub static DESC: FilterDesc = FilterDesc {
    name: "graphiceq",
    label: "Graphic EQ",
    description: "20-band graphic equalizer whose curve follows the sliders",
    params: PARAMS,
    graph: GraphKind::Frequency,
    meter: None,
    new: |fs, ch| Box::new(GraphicEq::new(fs as f64, ch)),
};

/// Centre frequency of band `k`: half-octave steps around 1 kHz.
fn centre(k: usize) -> f64 {
    1000.0 * libm::pow(2.0, (k as f64 - 11.0) / 2.0)
}

pub struct GraphicEq {
    fs: f64,
    channels: usize,
    /// Number of active bands (the lowest ones).
    active: usize,
    /// `centre(k)` per band, and the same as response probes for the solve.
    centres: [f64; BANDS],
    probes: [Probe; BANDS],
    sliders: [f64; BANDS],
    /// Solved filter gains in dB for `sliders`; `None` after a slider
    /// changed. A `Cell` because `graph` (`&self`) may solve first.
    solved: Cell<Option<[f64; BANDS]>>,
    /// Filter gains in dB, smoothed towards `solved`.
    gains: [Smoother; BANDS],
    output: Smoother,
    coefs: [Coefs; BANDS],
    /// Bands with a non-zero gain, i.e. that need processing.
    on: [bool; BANDS],
    states: Vec<[State; BANDS]>,
    countdown: u32,
    dirty: bool,
}

impl GraphicEq {
    fn new(fs: f64, channels: usize) -> Self {
        let control_rate = fs / CONTROL_FRAMES as f64;
        let centres: [f64; BANDS] = core::array::from_fn(centre);
        Self {
            fs,
            channels,
            active: centres.iter().take_while(|&&f| f < fs * MAX_BAND_RATIO).count(),
            centres,
            probes: centres.map(|f| Probe::new(fs, f)),
            sliders: [0.0; BANDS],
            solved: Cell::new(None),
            gains: [Smoother::new(0.0, control_rate); BANDS],
            output: Smoother::new(1.0, fs),
            coefs: [Coefs::IDENTITY; BANDS],
            on: [false; BANDS],
            states: vec![[State::default(); BANDS]; channels],
            countdown: 0,
            dirty: true,
        }
    }

    fn band(&self, k: usize, gain_db: f64) -> Coefs {
        if gain_db == 0.0 {
            Coefs::IDENTITY
        } else {
            Coefs::new(Shape::Peaking, self.fs, self.centres[k], Q, gain_db)
        }
    }

    /// Filter gains whose combined response hits every slider value at the
    /// band centres. A band's dB response is nearly, but not exactly,
    /// proportional to its gain, so the linear system is solved with each
    /// band's response per dB at the gain from the previous round.
    fn solve(&self) -> [f64; BANDS] {
        let n = self.active;
        let mut x = [0.0; BANDS];
        if self.sliders[..n].iter().all(|&s| s == 0.0) {
            return x;
        }
        x[..n].copy_from_slice(&self.sliders[..n]);
        for _ in 0..SOLVE_ROUNDS {
            let mut m = [[0.0; BANDS + 1]; BANDS];
            for j in 0..n {
                // Response per dB of band j, from a non-zero probe gain.
                let probe = if libm::fabs(x[j]) < 1.0 { 1.0 } else { x[j] };
                let c = self.band(j, probe);
                for (row, p) in m[..n].iter_mut().zip(&self.probes) {
                    row[j] = c.response_db_at(p) / probe;
                }
            }
            for (k, row) in m[..n].iter_mut().enumerate() {
                row[n] = self.sliders[k];
            }
            solve_linear(&mut m, n, &mut x);
            for g in &mut x[..n] {
                *g = g.clamp(-MAX_FILTER_GAIN, MAX_FILTER_GAIN);
            }
        }
        x
    }

    fn targets(&self) -> [f64; BANDS] {
        self.solved.get().unwrap_or_else(|| {
            let t = self.solve();
            self.solved.set(Some(t));
            t
        })
    }

    fn update_coefs(&mut self) {
        let targets = self.targets();
        for (s, t) in self.gains.iter_mut().zip(targets) {
            s.set(t);
        }
        let mut settled = true;
        for k in 0..self.active {
            let g = self.gains[k].next();
            settled &= self.gains[k].is_settled();
            self.coefs[k] = self.band(k, g);
            self.on[k] = g != 0.0;
            if g == 0.0 {
                // Bypassed bands keep no state, so re-enabling them starts clean.
                for st in &mut self.states {
                    st[k] = State::default();
                }
            }
        }
        self.dirty = !settled;
    }
}

/// Gaussian elimination with partial pivoting on the augmented `n × (n+1)`
/// matrix `m` (right-hand side in column `n`). Only basic arithmetic, so the
/// result is identical on all platforms.
fn solve_linear(m: &mut [[f64; BANDS + 1]; BANDS], n: usize, x: &mut [f64; BANDS]) {
    for col in 0..n {
        let pivot = (col..n)
            .max_by(|&a, &b| libm::fabs(m[a][col]).total_cmp(&libm::fabs(m[b][col])))
            .unwrap_or(col);
        m.swap(col, pivot);
        let p = m[col][col];
        if p == 0.0 {
            continue;
        }
        for row in col + 1..n {
            let f = m[row][col] / p;
            if f != 0.0 {
                for c in col..=n {
                    m[row][c] -= f * m[col][c];
                }
            }
        }
    }
    for row in (0..n).rev() {
        let mut v = m[row][n];
        for c in row + 1..n {
            v -= m[row][c] * x[c];
        }
        x[row] = if m[row][row] != 0.0 { v / m[row][row] } else { 0.0 };
    }
}

impl Filter for GraphicEq {
    fn set_param(&mut self, index: usize, value: f32) {
        let Some(info) = PARAMS.get(index) else { return };
        let v = info.sanitize(value) as f64;
        if index == OUTPUT {
            self.output.set(db_to_lin(v));
            return;
        }
        if self.sliders[index] != v {
            self.sliders[index] = v;
            self.solved.set(None);
            self.dirty = true;
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(self.channels) {
            if self.countdown == 0 {
                self.countdown = CONTROL_FRAMES;
                if self.dirty {
                    self.update_coefs();
                }
                for st in self.states.iter_mut().flatten() {
                    st.flush();
                }
            }
            self.countdown -= 1;

            let gain = self.output.next();
            for (s, state) in frame.iter_mut().zip(self.states.iter_mut()) {
                let mut x = *s as f64;
                for ((c, st), &on) in self.coefs[..self.active].iter().zip(state.iter_mut()).zip(&self.on) {
                    if on {
                        x = st.tick(c, x);
                    }
                }
                *s = (x * gain) as f32;
            }
        }
    }

    fn reset(&mut self) {
        let targets = self.targets();
        for (s, t) in self.gains.iter_mut().zip(targets) {
            s.set(t);
            s.snap();
        }
        self.output.snap();
        self.update_coefs();
        self.countdown = 0;
        for st in &mut self.states {
            *st = [State::default(); BANDS];
        }
    }

    fn graph(&self, x: f32) -> f32 {
        let targets = self.targets();
        let mut db = lin_to_db(self.output.target());
        for k in 0..self.active {
            let c = self.band(k, targets[k]);
            if c != Coefs::IDENTITY {
                db += c.response_db(self.fs, x as f64);
            }
        }
        db as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eq_with(fs: f32, sliders: &[f32]) -> Box<dyn Filter> {
        let mut eq = crate::create(&DESC, fs, 1);
        for (k, &v) in sliders.iter().enumerate() {
            eq.set_param(k, v);
        }
        eq
    }

    /// Largest deviation of the curve from the sliders at the band centres.
    fn centre_error(eq: &dyn Filter, sliders: &[f32], bands: usize) -> f32 {
        (0..bands).map(|k| (eq.graph(centre(k) as f32) - sliders[k]).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn curve_follows_sliders() {
        let patterns: [[f32; BANDS]; 5] = [
            [12.0; BANDS],
            [-12.0; BANDS],
            core::array::from_fn(|k| if k == 9 { 12.0 } else { 0.0 }),
            core::array::from_fn(|k| if (8..11).contains(&k) { -12.0 } else { 0.0 }),
            core::array::from_fn(|k| [6.0, 3.0, -4.0, 0.0, 8.0, -9.0, 2.0][k % 7]),
        ];
        for p in &patterns {
            let eq = eq_with(48000.0, p);
            let err = centre_error(&*eq, p, BANDS);
            assert!(err < 0.25, "{p:?}: {err} dB off");
        }
    }

    #[test]
    fn little_ripple_between_bands_when_flat() {
        let eq = eq_with(48000.0, &[12.0; BANDS]);
        for k in 1..BANDS {
            let between = libm::sqrt(centre(k - 1) * centre(k)) as f32;
            let d = eq.graph(between) - 12.0;
            assert!(d.abs() < 1.2, "{between} Hz: {d}");
        }
    }

    #[test]
    fn audio_matches_graph() {
        let mut sliders = [0.0f32; BANDS];
        sliders[11] = 9.0;
        sliders[12] = 9.0;
        let mut eq = eq_with(48000.0, &sliders);
        eq.reset();
        let f = 1000.0f32;
        let mut buf: Vec<f32> =
            (0..48000).map(|i| libm::sinf(2.0 * core::f32::consts::PI * f * i as f32 / 48000.0) * 0.1).collect();
        eq.process(&mut buf);
        let peak = buf[24000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let db = lin_to_db(peak as f64 / 0.1) as f32;
        assert!((db - eq.graph(f)).abs() < 0.05, "{db} vs {}", eq.graph(f));
        assert!((db - 9.0).abs() < 0.25, "{db}");
    }

    #[test]
    fn top_bands_inactive_at_low_sample_rates() {
        let mut eq = crate::create(&DESC, 32000.0, 1);
        eq.set_param(19, 12.0); // 16 kHz
        assert_eq!(eq.graph(10000.0), 0.0);
        let p = [5.0f32; BANDS];
        let eq = eq_with(32000.0, &p);
        assert!(centre_error(&*eq, &p, 19) < 0.25);
    }
}
