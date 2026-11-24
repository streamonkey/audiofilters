//! Parametric equalizer: high-pass (12/24 dB/oct), low shelf, three peaking
//! bands, high shelf and low-pass (12 dB/oct).
//!
//! Filter parameters are smoothed at the control rate
//! (`util::CONTROL_FRAMES`), the output gain per sample.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::f64::consts::FRAC_1_SQRT_2;

use crate::biquad::{Coefs, Shape, State};
use crate::util::{db_to_lin, lin_to_db, Smoother, CONTROL_FRAMES};
use crate::{Filter, FilterDesc, GraphKind, ParamInfo, ParamKind};

const HP_ENABLED: usize = 0;
const HP_FREQ: usize = 1;
const HP_SLOPE: usize = 2;
const LOW_FREQ: usize = 3;
const LOW_GAIN: usize = 4;
const BAND1: usize = 5; // freq, gain, q for bands 1..3
const HIGH_FREQ: usize = 14;
const HIGH_GAIN: usize = 15;
const LP_ENABLED: usize = 16;
const LP_FREQ: usize = 17;
const OUTPUT: usize = 18;

const N_PARAMS: usize = 19;

const PARAMS: &[ParamInfo; N_PARAMS] = &[
    ParamInfo::toggle("hp-enabled", "High-pass", true),
    ParamInfo::float("hp-freq", "High-pass freq", "Hz", 10.0, 1000.0, 60.0),
    ParamInfo::choice("hp-slope", "High-pass slope", &["12db", "24db"], 1),
    ParamInfo::float("low-freq", "Low shelf freq", "Hz", 20.0, 1000.0, 120.0),
    ParamInfo::float("low-gain", "Low shelf gain", "dB", -18.0, 18.0, 0.0),
    ParamInfo::float("band1-freq", "Band 1 freq", "Hz", 20.0, 20000.0, 250.0),
    ParamInfo::float("band1-gain", "Band 1 gain", "dB", -18.0, 18.0, 0.0),
    ParamInfo::float("band1-q", "Band 1 Q", "", 0.1, 10.0, 1.0),
    ParamInfo::float("band2-freq", "Band 2 freq", "Hz", 20.0, 20000.0, 1000.0),
    ParamInfo::float("band2-gain", "Band 2 gain", "dB", -18.0, 18.0, 0.0),
    ParamInfo::float("band2-q", "Band 2 Q", "", 0.1, 10.0, 1.0),
    ParamInfo::float("band3-freq", "Band 3 freq", "Hz", 20.0, 20000.0, 4000.0),
    ParamInfo::float("band3-gain", "Band 3 gain", "dB", -18.0, 18.0, 0.0),
    ParamInfo::float("band3-q", "Band 3 Q", "", 0.1, 10.0, 1.0),
    ParamInfo::float("high-freq", "High shelf freq", "Hz", 1000.0, 20000.0, 8000.0),
    ParamInfo::float("high-gain", "High shelf gain", "dB", -18.0, 18.0, 0.0),
    ParamInfo::toggle("lp-enabled", "Low-pass", false),
    ParamInfo::float("lp-freq", "Low-pass freq", "Hz", 1000.0, 22000.0, 18000.0),
    ParamInfo::float("output", "Output gain", "dB", -24.0, 24.0, 0.0),
];

pub static DESC: FilterDesc = FilterDesc {
    name: "eq",
    label: "Equalizer",
    description: "Parametric EQ with high-pass, shelves, three bands and low-pass",
    params: PARAMS,
    graph: GraphKind::Frequency,
    meter: None,
    new: |fs, ch| Box::new(Equalizer::new(fs as f64, ch)),
};

const SECTIONS: usize = 8;

/// Butterworth Q values for one 2nd order and two cascaded sections.
const Q_BW2: f64 = FRAC_1_SQRT_2;
const Q_BW4: [f64; 2] = [0.541_196_100_146_197, 1.306_562_964_876_376_5];

fn section_coefs(fs: f64, v: impl Fn(usize) -> f64) -> [Coefs; SECTIONS] {
    let mut c = [Coefs::IDENTITY; SECTIONS];
    let gain_section = |shape, f, q, g: f64| {
        if g == 0.0 {
            Coefs::IDENTITY
        } else {
            Coefs::new(shape, fs, f, q, g)
        }
    };

    if v(HP_ENABLED) != 0.0 {
        if v(HP_SLOPE) == 0.0 {
            c[0] = Coefs::new(Shape::HighPass, fs, v(HP_FREQ), Q_BW2, 0.0);
        } else {
            c[0] = Coefs::new(Shape::HighPass, fs, v(HP_FREQ), Q_BW4[0], 0.0);
            c[1] = Coefs::new(Shape::HighPass, fs, v(HP_FREQ), Q_BW4[1], 0.0);
        }
    }
    c[2] = gain_section(Shape::LowShelf, v(LOW_FREQ), Q_BW2, v(LOW_GAIN));
    for b in 0..3 {
        let i = BAND1 + 3 * b;
        c[3 + b] = gain_section(Shape::Peaking, v(i), v(i + 2), v(i + 1));
    }
    c[6] = gain_section(Shape::HighShelf, v(HIGH_FREQ), Q_BW2, v(HIGH_GAIN));
    if v(LP_ENABLED) != 0.0 {
        c[7] = Coefs::new(Shape::LowPass, fs, v(LP_FREQ), Q_BW2, 0.0);
    }
    c
}

pub struct Equalizer {
    fs: f64,
    channels: usize,
    /// All parameters except the output gain, which has its own smoother.
    params: [Smoother; OUTPUT],
    output: Smoother,
    coefs: [Coefs; SECTIONS],
    states: Vec<[State; SECTIONS]>,
    countdown: u32,
    dirty: bool,
}

impl Equalizer {
    fn new(fs: f64, channels: usize) -> Self {
        let control_rate = fs / CONTROL_FRAMES as f64;
        Self {
            fs,
            channels,
            params: [Smoother::new(0.0, control_rate); OUTPUT],
            output: Smoother::new(1.0, fs),
            coefs: [Coefs::IDENTITY; SECTIONS],
            states: vec![[State::default(); SECTIONS]; channels],
            countdown: 0,
            dirty: true,
        }
    }

    fn update_coefs(&mut self) {
        let mut values = [0.0; OUTPUT];
        let mut settled = true;
        for (v, s) in values.iter_mut().zip(self.params.iter_mut()) {
            *v = s.next();
            settled &= s.is_settled();
        }
        self.coefs = section_coefs(self.fs, |i| values[i]);
        // Bypassed sections keep no state, so re-enabling them starts clean.
        for (k, c) in self.coefs.iter().enumerate() {
            if *c == Coefs::IDENTITY {
                for st in &mut self.states {
                    st[k] = State::default();
                }
            }
        }
        self.dirty = !settled;
    }
}

impl Filter for Equalizer {
    fn set_param(&mut self, index: usize, value: f32) {
        let Some(info) = PARAMS.get(index) else { return };
        let v = info.sanitize(value) as f64;
        if index == OUTPUT {
            self.output.set(db_to_lin(v));
            return;
        }
        self.params[index].set(v);
        if info.kind != ParamKind::Float {
            self.params[index].snap();
        }
        self.dirty = true;
    }

    fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(self.channels) {
            if self.countdown == 0 {
                self.countdown = CONTROL_FRAMES;
                if self.dirty {
                    self.update_coefs();
                }
            }
            self.countdown -= 1;

            let gain = self.output.next();
            for (s, state) in frame.iter_mut().zip(self.states.iter_mut()) {
                let mut x = *s as f64;
                for (c, st) in self.coefs.iter().zip(state.iter_mut()) {
                    if *c != Coefs::IDENTITY {
                        x = st.tick(c, x);
                    }
                }
                *s = (x * gain) as f32;
            }
        }
    }

    fn reset(&mut self) {
        for s in &mut self.params {
            s.snap();
        }
        self.output.snap();
        self.update_coefs();
        self.countdown = 0;
        for st in &mut self.states {
            *st = [State::default(); SECTIONS];
        }
    }

    fn graph(&self, x: f32) -> f32 {
        let coefs = section_coefs(self.fs, |i| self.params[i].target());
        let mut db = lin_to_db(self.output.target());
        for c in coefs.iter().filter(|c| **c != Coefs::IDENTITY) {
            db += c.response_db(self.fs, x as f64);
        }
        db as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_by_default_above_highpass() {
        let eq = crate::create(&DESC, 48000.0, 2);
        assert!(eq.graph(1000.0).abs() < 0.001);
        assert!(eq.graph(30.0) < -6.0);
    }

    #[test]
    fn band_gain_shows_in_graph_and_audio() {
        let mut eq = crate::create(&DESC, 48000.0, 1);
        eq.set_param(BAND1 + 3 + 1, 6.0); // band2-gain at 1 kHz
        eq.reset();
        assert!((eq.graph(1000.0) - 6.0).abs() < 0.01);
        let mut buf: Vec<f32> = (0..48000)
            .map(|i| libm::sinf(2.0 * core::f32::consts::PI * 1000.0 * i as f32 / 48000.0) * 0.1)
            .collect();
        eq.process(&mut buf);
        let peak = buf[24000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let db = crate::util::lin_to_db(peak as f64 / 0.1);
        assert!((db - 6.0).abs() < 0.05, "{db}");
    }
}
