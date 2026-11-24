//! Stereo utility: gain, width, balance, mono, channel swap and polarity
//! inversion, with a phase correlation meter. For channel counts other than
//! two only the gain is applied.

use alloc::boxed::Box;

use crate::util::{db_to_lin, flush, time_coef, Smoother};
use crate::{Filter, FilterDesc, GraphKind, ParamInfo};

const GAIN: usize = 0;
const WIDTH: usize = 1;
const BALANCE: usize = 2;
const MONO: usize = 3;
const SWAP: usize = 4;
const INVERT_LEFT: usize = 5;
const INVERT_RIGHT: usize = 6;

const PARAMS: &[ParamInfo] = &[
    ParamInfo::float("gain", "Gain", "dB", -24.0, 24.0, 0.0),
    ParamInfo::float("width", "Width", "%", 0.0, 200.0, 100.0),
    ParamInfo::float("balance", "Balance", "%", -100.0, 100.0, 0.0),
    ParamInfo::toggle("mono", "Mono", false),
    ParamInfo::toggle("swap", "Swap channels", false),
    ParamInfo::toggle("invert-left", "Invert left", false),
    ParamInfo::toggle("invert-right", "Invert right", false),
];

pub static DESC: FilterDesc = FilterDesc {
    name: "stereo",
    label: "Stereo tool",
    description: "Gain, width, balance, mono, swap and polarity",
    params: PARAMS,
    graph: GraphKind::None,
    meter: Some(("correlation", "")),
    new: |fs, ch| Box::new(StereoTool::new(fs as f64, ch)),
};

/// Integration time of the correlation meter.
const CORRELATION_MS: f64 = 300.0;

pub struct StereoTool {
    channels: usize,
    gain: Smoother,
    width: Smoother,
    balance: Smoother,
    width_target: f64,
    mono: bool,
    /// 0 = straight, 1 = swapped; crossfaded so toggling does not click.
    swap: Smoother,
    /// Polarity factors, ramped through zero when toggled.
    invert: [Smoother; 2],
    corr_coef: f64,
    lr: f64,
    ll: f64,
    rr: f64,
}

impl StereoTool {
    fn new(fs: f64, channels: usize) -> Self {
        Self {
            channels,
            gain: Smoother::new(1.0, fs),
            width: Smoother::new(1.0, fs),
            balance: Smoother::new(0.0, fs),
            width_target: 1.0,
            mono: false,
            swap: Smoother::new(0.0, fs),
            invert: [Smoother::new(1.0, fs); 2],
            corr_coef: time_coef(CORRELATION_MS, fs),
            lr: 0.0,
            ll: 0.0,
            rr: 0.0,
        }
    }
}

impl Filter for StereoTool {
    fn set_param(&mut self, index: usize, value: f32) {
        let Some(info) = PARAMS.get(index) else { return };
        let v = info.sanitize(value) as f64;
        match index {
            GAIN => self.gain.set(db_to_lin(v)),
            WIDTH => self.width_target = v / 100.0,
            BALANCE => self.balance.set(v / 100.0),
            MONO => self.mono = v != 0.0,
            SWAP => self.swap.set(v),
            INVERT_LEFT => self.invert[0].set(1.0 - 2.0 * v),
            INVERT_RIGHT => self.invert[1].set(1.0 - 2.0 * v),
            _ => {}
        }
        // Mono is a smoothed width of zero, so toggling it does not click.
        self.width.set(if self.mono { 0.0 } else { self.width_target });
    }

    fn process(&mut self, buf: &mut [f32]) {
        if self.channels != 2 {
            for frame in buf.chunks_exact_mut(self.channels) {
                let gain = self.gain.next();
                for s in frame.iter_mut() {
                    *s = (*s as f64 * gain) as f32;
                }
            }
            return;
        }
        let c = self.corr_coef;
        for frame in buf.chunks_exact_mut(2) {
            let gain = self.gain.next();
            let width = self.width.next();
            let balance = self.balance.next();

            let swap = self.swap.next();
            let (x0, x1) = (frame[0] as f64, frame[1] as f64);
            let mut l = (x0 + (x1 - x0) * swap) * self.invert[0].next();
            let mut r = (x1 + (x0 - x1) * swap) * self.invert[1].next();

            let mid = (l + r) * 0.5;
            let side = (l - r) * 0.5 * width;
            l = mid + side;
            r = mid - side;

            if balance > 0.0 {
                l *= 1.0 - balance;
            } else {
                r *= 1.0 + balance;
            }
            l *= gain;
            r *= gain;

            self.lr = flush(l * r + (self.lr - l * r) * c);
            self.ll = flush(l * l + (self.ll - l * l) * c);
            self.rr = flush(r * r + (self.rr - r * r) * c);

            frame[0] = l as f32;
            frame[1] = r as f32;
        }
    }

    fn reset(&mut self) {
        self.gain.snap();
        self.width.snap();
        self.balance.snap();
        self.swap.snap();
        self.invert.iter_mut().for_each(Smoother::snap);
        self.lr = 0.0;
        self.ll = 0.0;
        self.rr = 0.0;
    }

    /// Phase correlation: +1 mono compatible, 0 uncorrelated, -1 out of phase.
    fn meter(&self) -> f32 {
        let norm = libm::sqrt(self.ll * self.rr);
        if norm < 1e-12 {
            1.0
        } else {
            (self.lr / norm) as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_and_correlation() {
        let mut st = crate::create(&DESC, 48000.0, 2);
        let mut buf: alloc::vec::Vec<f32> = (0..48000).flat_map(|i| {
            let x = libm::sinf(i as f32 * 0.05) * 0.5;
            [x, -x]
        }).collect();
        st.process(&mut buf);
        assert!(st.meter() < -0.99);
        st.set_param(MONO, 1.0);
        st.process(&mut buf);
        // fully out-of-phase content cancels in mono
        assert!(buf[buf.len() - 2].abs() < 1e-6);
    }
}
