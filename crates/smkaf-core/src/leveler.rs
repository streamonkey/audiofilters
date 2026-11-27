//! Slow automatic gain control ("leveler") for evening out loudness between
//! sources, e.g. different songs, jingles and microphone.
//!
//! The RMS level is compared to a target and the gain moves towards the
//! difference with separate time constants for raising and lowering. Below
//! the freeze threshold the gain is held, so pauses and fades do not get
//! pumped up.

use alloc::boxed::Box;

use crate::util::{flush, mean_square, pow_to_db, time_coef, DbToLin};
use crate::{Filter, FilterDesc, GraphKind, ParamInfo};

const TARGET: usize = 0;
const MAX_GAIN: usize = 1;
const MAX_CUT: usize = 2;
const RAISE_TIME: usize = 3;
const LOWER_TIME: usize = 4;
const FREEZE: usize = 5;
const WINDOW: usize = 6;

const PARAMS: &[ParamInfo] = &[
    ParamInfo::float("target", "Target level", "dB", -40.0, 0.0, -18.0),
    ParamInfo::float("max-gain", "Max gain", "dB", 0.0, 30.0, 12.0),
    ParamInfo::float("max-cut", "Max cut", "dB", 0.0, 30.0, 12.0),
    ParamInfo::float("raise-time", "Raise time", "s", 0.5, 60.0, 8.0),
    ParamInfo::float("lower-time", "Lower time", "s", 0.1, 30.0, 2.0),
    ParamInfo::float("freeze", "Freeze below", "dB", -90.0, -10.0, -45.0),
    ParamInfo::float("window", "RMS window", "ms", 50.0, 3000.0, 400.0),
];

pub static DESC: FilterDesc = FilterDesc {
    name: "leveler",
    label: "Leveler",
    description: "Slow automatic gain control with freeze gate",
    params: PARAMS,
    graph: GraphKind::Transfer,
    meter: Some(("gain", "dB")),
    new: |fs, ch| Box::new(Leveler::new(fs as f64, ch)),
};

pub struct Leveler {
    fs: f64,
    channels: usize,
    target: f64,
    max_gain: f64,
    max_cut: f64,
    raise: f64,
    lower: f64,
    freeze: f64,
    window: f64,
    power: f64,
    gain: f64,
    gain_lin: DbToLin,
}

impl Leveler {
    fn new(fs: f64, channels: usize) -> Self {
        Self {
            fs,
            channels,
            target: 0.0,
            max_gain: 0.0,
            max_cut: 0.0,
            raise: 0.0,
            lower: 0.0,
            freeze: 0.0,
            window: 0.0,
            power: 0.0,
            gain: 0.0,
            gain_lin: DbToLin::new(),
        }
    }

    fn wanted(&self, level: f64) -> f64 {
        (self.target - level).clamp(-self.max_cut, self.max_gain)
    }
}

impl Filter for Leveler {
    fn set_param(&mut self, index: usize, value: f32) {
        let Some(info) = PARAMS.get(index) else { return };
        let v = info.sanitize(value) as f64;
        match index {
            TARGET => self.target = v,
            MAX_GAIN => self.max_gain = v,
            MAX_CUT => self.max_cut = v,
            RAISE_TIME => self.raise = time_coef(v * 1000.0, self.fs),
            LOWER_TIME => self.lower = time_coef(v * 1000.0, self.fs),
            FREEZE => self.freeze = v,
            WINDOW => self.window = time_coef(v, self.fs),
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(self.channels) {
            let sq = mean_square(frame);
            self.power = flush(sq + (self.power - sq) * self.window);
            let level = pow_to_db(self.power);

            if level >= self.freeze {
                let wanted = self.wanted(level);
                let c = if wanted > self.gain { self.raise } else { self.lower };
                self.gain = flush(wanted + (self.gain - wanted) * c);
            }
            let g = self.gain_lin.get(self.gain);
            for s in frame.iter_mut() {
                *s = (*s as f64 * g) as f32;
            }
        }
    }

    fn reset(&mut self) {
        self.power = 0.0;
        self.gain = 0.0;
    }

    fn meter(&self) -> f32 {
        self.gain as f32
    }

    /// Steady-state curve. Ignores the freeze level, below which the gain
    /// stays wherever it was.
    fn graph(&self, x: f32) -> f32 {
        let x = x as f64;
        (x + self.wanted(x)) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::db_to_lin;

    #[test]
    fn converges_to_target_and_freezes() {
        let mut l = crate::create(&DESC, 48000.0, 1);
        // -30 dBFS RMS input should be raised by the full 12 dB.
        let amp = db_to_lin(-30.0) as f32;
        let mut buf = alloc::vec![amp; 48000 * 60];
        l.process(&mut buf);
        assert!((l.meter() - 12.0).abs() < 0.1, "{}", l.meter());
        let mut silence = alloc::vec![0.0f32; 48000 * 10];
        l.process(&mut silence);
        assert!((l.meter() - 12.0).abs() < 0.1, "gain must be frozen in silence");
    }
}
