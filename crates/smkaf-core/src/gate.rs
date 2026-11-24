//! Noise gate with hysteresis, hold time and adjustable range (a range above
//! -90 dB turns it into a downward expander-style "duck" instead of a hard
//! mute). Always stereo-linked.

use alloc::boxed::Box;

use crate::util::{ballistics, db_to_lin, flush, lin_to_db, peak, time_coef};
use crate::{Filter, FilterDesc, GraphKind, ParamInfo};

const THRESHOLD: usize = 0;
const HYSTERESIS: usize = 1;
const RANGE: usize = 2;
const ATTACK: usize = 3;
const HOLD: usize = 4;
const RELEASE: usize = 5;

const PARAMS: &[ParamInfo] = &[
    ParamInfo::float("threshold", "Threshold", "dB", -80.0, 0.0, -50.0),
    ParamInfo::float("hysteresis", "Hysteresis", "dB", 0.0, 20.0, 4.0),
    ParamInfo::float("range", "Range", "dB", -90.0, 0.0, -40.0),
    ParamInfo::float("attack", "Attack", "ms", 0.05, 50.0, 0.5),
    ParamInfo::float("hold", "Hold", "ms", 0.0, 1000.0, 50.0),
    ParamInfo::float("release", "Release", "ms", 5.0, 3000.0, 200.0),
];

pub static DESC: FilterDesc = FilterDesc {
    name: "gate",
    label: "Gate",
    description: "Noise gate with hysteresis and hold",
    params: PARAMS,
    graph: GraphKind::Transfer,
    meter: Some(("gain", "dB")),
    new: |fs, ch| Box::new(Gate::new(fs as f64, ch)),
};

/// Decay of the peak envelope, so the gate does not chatter on zero crossings.
const ENVELOPE_MS: f64 = 10.0;

pub struct Gate {
    fs: f64,
    channels: usize,
    threshold: f64,
    hysteresis: f64,
    range: f64,
    attack: f64,
    release: f64,
    hold_frames: u32,
    env_coef: f64,
    env: f64,
    open: bool,
    hold: u32,
    gain: f64,
}

impl Gate {
    fn new(fs: f64, channels: usize) -> Self {
        Self {
            fs,
            channels,
            threshold: 0.0,
            hysteresis: 0.0,
            range: 0.0,
            attack: 0.0,
            release: 0.0,
            hold_frames: 0,
            env_coef: time_coef(ENVELOPE_MS, fs),
            env: 0.0,
            open: false,
            hold: 0,
            gain: 0.0,
        }
    }
}

impl Filter for Gate {
    fn set_param(&mut self, index: usize, value: f32) {
        let Some(info) = PARAMS.get(index) else { return };
        let v = info.sanitize(value) as f64;
        match index {
            THRESHOLD => self.threshold = v,
            HYSTERESIS => self.hysteresis = v,
            RANGE => self.range = v,
            ATTACK => self.attack = time_coef(v, self.fs),
            HOLD => self.hold_frames = (v * self.fs / 1000.0) as u32,
            RELEASE => self.release = time_coef(v, self.fs),
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(self.channels) {
            let peak = peak(frame);
            self.env = if peak > self.env { peak } else { flush(self.env * self.env_coef) };
            let level = lin_to_db(self.env);

            if !self.open {
                if level > self.threshold {
                    self.open = true;
                    self.hold = self.hold_frames;
                }
            } else if level >= self.threshold - self.hysteresis {
                self.hold = self.hold_frames;
            } else if self.hold > 0 {
                self.hold -= 1;
            } else {
                self.open = false;
            }

            let target = if self.open { 0.0 } else { self.range };
            // A falling gain means the gate is closing, so that is the release.
            self.gain = ballistics(self.gain, target, self.release, self.attack);
            let g = db_to_lin(self.gain);
            for s in frame.iter_mut() {
                *s = (*s as f64 * g) as f32;
            }
        }
    }

    fn reset(&mut self) {
        self.env = 0.0;
        self.open = false;
        self.hold = 0;
        self.gain = self.range;
    }

    fn meter(&self) -> f32 {
        self.gain as f32
    }

    fn graph(&self, x: f32) -> f32 {
        let x = x as f64;
        (if x >= self.threshold { x } else { x + self.range }) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_closes() {
        let mut g = crate::create(&DESC, 48000.0, 1);
        let mut quiet = alloc::vec![db_to_lin(-70.0) as f32; 48000];
        g.process(&mut quiet);
        assert!((g.meter() - -40.0).abs() < 0.01);
        let mut loud = alloc::vec![0.5f32; 4800];
        g.process(&mut loud);
        assert!(g.meter() > -0.01);
        // envelope decay + hold + ~8 release time constants
        let mut quiet = alloc::vec![db_to_lin(-70.0) as f32; 2 * 48000];
        g.process(&mut quiet);
        assert!((g.meter() - -40.0).abs() < 0.01);
    }

    #[test]
    fn closes_again_after_infinite_input() {
        let mut g = crate::create(&DESC, 48000.0, 1);
        let mut buf = alloc::vec![db_to_lin(-70.0) as f32; 2 * 48000];
        buf[0] = f32::INFINITY;
        g.process(&mut buf);
        assert!((g.meter() - -40.0).abs() < 0.01);
    }
}
