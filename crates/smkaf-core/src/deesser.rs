//! De-esser: compresses sibilance detected above a split frequency.
//!
//! In `split` mode only the high band (complementary split, so the bands sum
//! back to the input exactly when no reduction happens) is attenuated; in
//! `wide` mode the whole signal is. `listen` outputs the sidechain.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::f64::consts::FRAC_1_SQRT_2;

use crate::biquad::{Coefs, Shape, State};
use crate::compressor::gain_computer;
use crate::util::{ballistics, db_to_lin, lin_to_db, time_coef, DbToLin, Smoother, CONTROL_FRAMES};
use crate::{Filter, FilterDesc, GraphKind, ParamInfo};

const FREQUENCY: usize = 0;
const THRESHOLD: usize = 1;
const RATIO: usize = 2;
const MAX_REDUCTION: usize = 3;
const RELEASE: usize = 4;
const MODE: usize = 5;
const LISTEN: usize = 6;

const PARAMS: &[ParamInfo] = &[
    ParamInfo::float("frequency", "Frequency", "Hz", 2000.0, 12000.0, 5500.0),
    ParamInfo::float("threshold", "Threshold", "dB", -60.0, 0.0, -30.0),
    ParamInfo::float("ratio", "Ratio", ":1", 1.0, 20.0, 4.0),
    ParamInfo::float("max-reduction", "Max reduction", "dB", 0.0, 24.0, 12.0),
    ParamInfo::float("release", "Release", "ms", 10.0, 500.0, 60.0),
    ParamInfo::choice("mode", "Mode", &["split", "wide"], 0),
    ParamInfo::toggle("listen", "Listen to sidechain", false),
];

pub static DESC: FilterDesc = FilterDesc {
    name: "deesser",
    label: "De-esser",
    description: "Split-band or wideband de-esser",
    params: PARAMS,
    graph: GraphKind::Transfer,
    meter: Some(("gain-reduction", "dB")),
    new: |fs, ch| Box::new(DeEsser::new(fs as f64, ch)),
};

const ATTACK_MS: f64 = 0.5;

fn highpass(fs: f64, freq: f64) -> Coefs {
    Coefs::new(Shape::HighPass, fs, freq, FRAC_1_SQRT_2, 0.0)
}

pub struct DeEsser {
    fs: f64,
    channels: usize,
    threshold: f64,
    /// `threshold` as amplitude: below it there is no reduction, so most
    /// frames need no `log10`.
    threshold_lin: f64,
    ratio: f64,
    max_reduction: f64,
    attack: f64,
    release: f64,
    split: bool,
    listen: bool,
    /// Split frequency, smoothed at the control rate.
    frequency: Smoother,
    countdown: u32,
    coefs: Coefs,
    states: Vec<State>,
    highs: Vec<f64>,
    gain: f64,
    gain_lin: DbToLin,
    meter: f64,
}

impl DeEsser {
    fn new(fs: f64, channels: usize) -> Self {
        Self {
            fs,
            channels,
            threshold: 0.0,
            threshold_lin: 1.0,
            ratio: 1.0,
            max_reduction: 0.0,
            attack: time_coef(ATTACK_MS, fs),
            release: 0.0,
            split: true,
            listen: false,
            frequency: Smoother::new(0.0, fs / CONTROL_FRAMES as f64),
            countdown: 0,
            coefs: Coefs::IDENTITY,
            states: vec![State::default(); channels],
            highs: vec![0.0; channels],
            gain: 0.0,
            gain_lin: DbToLin::new(),
            meter: 0.0,
        }
    }

    fn reduction(&self, level: f64) -> f64 {
        (gain_computer(level, self.threshold, self.ratio, 0.0) - level).max(-self.max_reduction)
    }
}

impl Filter for DeEsser {
    fn set_param(&mut self, index: usize, value: f32) {
        let Some(info) = PARAMS.get(index) else { return };
        let v = info.sanitize(value) as f64;
        match index {
            FREQUENCY => self.frequency.set(v),
            THRESHOLD => {
                self.threshold = v;
                self.threshold_lin = db_to_lin(v);
            }
            RATIO => self.ratio = v,
            MAX_REDUCTION => self.max_reduction = v,
            RELEASE => self.release = time_coef(v, self.fs),
            MODE => self.split = v == 0.0,
            LISTEN => self.listen = v != 0.0,
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let mut meter = 0.0f64;
        for frame in buf.chunks_exact_mut(self.channels) {
            if self.countdown == 0 {
                self.countdown = CONTROL_FRAMES;
                if !self.frequency.is_settled() {
                    self.coefs = highpass(self.fs, self.frequency.next());
                }
                self.states.iter_mut().for_each(State::flush);
            }
            self.countdown -= 1;

            let mut peak = 0.0f64;
            for (c, s) in frame.iter().enumerate() {
                let hi = self.states[c].tick(&self.coefs, *s as f64);
                self.highs[c] = hi;
                let a = libm::fabs(hi);
                if a > peak {
                    peak = a;
                }
            }
            let target = if peak <= self.threshold_lin { 0.0 } else { self.reduction(lin_to_db(peak)) };
            self.gain = ballistics(self.gain, target, self.attack, self.release);
            if self.gain < meter {
                meter = self.gain;
            }
            let g = self.gain_lin.get(self.gain);

            for (s, &hi) in frame.iter_mut().zip(self.highs.iter()) {
                let x = *s as f64;
                *s = if self.listen {
                    hi
                } else if self.split {
                    (x - hi) + hi * g
                } else {
                    x * g
                } as f32;
            }
        }
        self.meter = meter;
    }

    fn reset(&mut self) {
        self.frequency.snap();
        self.coefs = highpass(self.fs, self.frequency.target());
        self.countdown = 0;
        self.states.fill(State::default());
        self.gain = 0.0;
        self.meter = 0.0;
    }

    fn meter(&self) -> f32 {
        self.meter as f32
    }

    /// Static curve of the high band: sidechain level in -> level out.
    fn graph(&self, x: f32) -> f32 {
        let x = x as f64;
        (x + self.reduction(x)) as f32
    }
}
