//! Feed-forward soft-knee compressor with peak or RMS detection, optional
//! stereo link and dry/wet mix (parallel compression).

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use crate::util::{ballistics, db_to_lin, flush, lin_to_db, mean_square, peak, pow_to_db, time_coef, Smoother};
use crate::{Filter, FilterDesc, GraphKind, ParamInfo};

const THRESHOLD: usize = 0;
const RATIO: usize = 1;
const KNEE: usize = 2;
const ATTACK: usize = 3;
const RELEASE: usize = 4;
const MAKEUP: usize = 5;
const MIX: usize = 6;
const DETECTOR: usize = 7;
const STEREO_LINK: usize = 8;

const PARAMS: &[ParamInfo] = &[
    ParamInfo::float("threshold", "Threshold", "dB", -60.0, 0.0, -18.0),
    ParamInfo::float("ratio", "Ratio", ":1", 1.0, 20.0, 4.0),
    ParamInfo::float("knee", "Knee", "dB", 0.0, 24.0, 6.0),
    ParamInfo::float("attack", "Attack", "ms", 0.1, 200.0, 10.0),
    ParamInfo::float("release", "Release", "ms", 5.0, 2000.0, 150.0),
    ParamInfo::float("makeup", "Makeup gain", "dB", -12.0, 24.0, 0.0),
    ParamInfo::float("mix", "Mix", "%", 0.0, 100.0, 100.0),
    ParamInfo::choice("detector", "Detector", &["peak", "rms"], 0),
    ParamInfo::toggle("stereo-link", "Stereo link", true),
];

pub static DESC: FilterDesc = FilterDesc {
    name: "compressor",
    label: "Compressor",
    description: "Soft-knee feed-forward compressor",
    params: PARAMS,
    graph: GraphKind::Transfer,
    meter: Some(("gain-reduction", "dB")),
    new: |fs, ch| Box::new(Compressor::new(fs as f64, ch)),
};

/// RMS detector window.
const RMS_MS: f64 = 10.0;

/// Static gain computer: input level -> output level (both dB).
pub fn gain_computer(x: f64, threshold: f64, ratio: f64, knee: f64) -> f64 {
    let over = x - threshold;
    if 2.0 * over < -knee {
        x
    } else if knee > 0.0 && 2.0 * over <= knee {
        let k = over + knee / 2.0;
        x + (1.0 / ratio - 1.0) * k * k / (2.0 * knee)
    } else {
        threshold + over / ratio
    }
}

pub struct Compressor {
    fs: f64,
    channels: usize,
    threshold: Smoother,
    ratio: Smoother,
    knee: Smoother,
    makeup: Smoother,
    mix: Smoother,
    attack: f64,
    release: f64,
    rms_coef: f64,
    rms: bool,
    link: bool,
    /// Per detector (1 when linked, else per channel): smoothed gain in dB.
    gain: Vec<f64>,
    /// Per detector: mean square for the RMS detector.
    power: Vec<f64>,
    meter: f64,
}

impl Compressor {
    fn new(fs: f64, channels: usize) -> Self {
        Self {
            fs,
            channels,
            threshold: Smoother::new(0.0, fs),
            ratio: Smoother::new(1.0, fs),
            knee: Smoother::new(0.0, fs),
            makeup: Smoother::new(0.0, fs),
            mix: Smoother::new(1.0, fs),
            attack: 0.0,
            release: 0.0,
            rms_coef: time_coef(RMS_MS, fs),
            rms: false,
            link: true,
            gain: vec![0.0; channels],
            power: vec![0.0; channels],
            meter: 0.0,
        }
    }

    /// Update detector `d` with input `x` and return its smoothed gain
    /// change in dB.
    #[inline]
    fn gain_db(&mut self, d: usize, x: f64, threshold: f64, ratio: f64, knee: f64) -> f64 {
        let level = if self.rms {
            let sq = x * x;
            self.power[d] = flush(sq + (self.power[d] - sq) * self.rms_coef);
            pow_to_db(self.power[d])
        } else {
            lin_to_db(libm::fabs(x))
        };
        let target = gain_computer(level, threshold, ratio, knee) - level;
        self.gain[d] = ballistics(self.gain[d], target, self.attack, self.release);
        self.gain[d]
    }
}

impl Filter for Compressor {
    fn set_param(&mut self, index: usize, value: f32) {
        let Some(info) = PARAMS.get(index) else { return };
        let v = info.sanitize(value) as f64;
        match index {
            THRESHOLD => self.threshold.set(v),
            RATIO => self.ratio.set(v),
            KNEE => self.knee.set(v),
            ATTACK => self.attack = time_coef(v, self.fs),
            RELEASE => self.release = time_coef(v, self.fs),
            MAKEUP => self.makeup.set(v),
            MIX => self.mix.set(v / 100.0),
            DETECTOR => self.rms = v != 0.0,
            STEREO_LINK => {
                let link = v != 0.0;
                if link && !self.link {
                    // Continue from the channel with the most reduction.
                    self.gain[0] = self.gain.iter().copied().fold(0.0, f64::min);
                    self.power[0] = self.power.iter().copied().fold(0.0, f64::max);
                } else if !link && self.link {
                    let (gain, power) = (self.gain[0], self.power[0]);
                    self.gain.fill(gain);
                    self.power.fill(power);
                }
                self.link = link;
            }
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let ch = self.channels;
        let mut meter = 0.0f64;
        for frame in buf.chunks_exact_mut(ch) {
            let t = self.threshold.next();
            let r = self.ratio.next();
            let k = self.knee.next();
            let makeup = self.makeup.next();
            let mix = self.mix.next();

            let wet = |g: f64| (1.0 - mix) + db_to_lin(g + makeup) * mix;

            if self.link {
                // The loudest channel (peak) or the mean power (RMS) drives one
                // shared gain.
                let x = if self.rms {
                    libm::sqrt(mean_square(frame))
                } else {
                    peak(frame)
                };
                let g = self.gain_db(0, x, t, r, k);
                meter = meter.min(g);
                let gain = wet(g);
                for s in frame.iter_mut() {
                    *s = (*s as f64 * gain) as f32;
                }
            } else {
                for (c, s) in frame.iter_mut().enumerate() {
                    let x = *s as f64;
                    let g = self.gain_db(c, x, t, r, k);
                    meter = meter.min(g);
                    *s = (x * wet(g)) as f32;
                }
            }
        }
        self.meter = meter;
    }

    fn reset(&mut self) {
        self.gain.fill(0.0);
        self.power.fill(0.0);
        self.meter = 0.0;
        for s in [&mut self.threshold, &mut self.ratio, &mut self.knee, &mut self.makeup, &mut self.mix] {
            s.snap();
        }
    }

    fn meter(&self) -> f32 {
        self.meter as f32
    }

    fn graph(&self, x: f32) -> f32 {
        let x = x as f64;
        let y = gain_computer(x, self.threshold.target(), self.ratio.target(), self.knee.target())
            + self.makeup.target();
        let mix = self.mix.target();
        lin_to_db((1.0 - mix) * db_to_lin(x) + mix * db_to_lin(y)) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_computer_regions() {
        // below the knee: unity
        assert_eq!(gain_computer(-40.0, -20.0, 4.0, 6.0), -40.0);
        // far above: ratio applies
        assert!((gain_computer(0.0, -20.0, 4.0, 6.0) - -15.0).abs() < 1e-12);
        // knee is continuous at both edges
        let lo = gain_computer(-23.0, -20.0, 4.0, 6.0);
        assert!((lo - -23.0).abs() < 1e-12);
        let hi = gain_computer(-17.0, -20.0, 4.0, 6.0);
        assert!((hi - (-20.0 + 3.0 / 4.0)).abs() < 1e-12);
    }

    #[test]
    fn steady_state_reduction() {
        let mut c = crate::create(&DESC, 48000.0, 1);
        c.set_param(THRESHOLD, -20.0);
        c.set_param(RATIO, 4.0);
        c.set_param(KNEE, 0.0);
        c.set_param(DETECTOR, 0.0);
        c.reset();
        // DC at -8 dBFS: expected output -20 + 12/4 = -17 dBFS
        let mut buf = vec![db_to_lin(-8.0) as f32; 48000];
        c.process(&mut buf);
        let out = lin_to_db(*buf.last().unwrap() as f64);
        assert!((out - -17.0).abs() < 0.01, "{out}");
        assert!((c.meter() - -9.0).abs() < 0.01);
    }
}
