//! Second-order IIR sections. Peaking filters use Vicanek's matched design,
//! the other shapes the RBJ audio EQ cookbook.

use core::f64::consts::PI;

use crate::util::{db_to_lin, flush, lin_to_db};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    LowPass,
    HighPass,
    Peaking,
    LowShelf,
    HighShelf,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coefs {
    pub const IDENTITY: Coefs = Coefs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    pub fn new(shape: Shape, sample_rate: f64, freq: f64, q: f64, gain_db: f64) -> Self {
        let freq = freq.min(sample_rate * 0.49).max(1.0);
        if shape == Shape::Peaking {
            return matched_peaking(2.0 * PI * freq / sample_rate, q.max(0.01), gain_db);
        }
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin, cos) = (libm::sin(w0), libm::cos(w0));
        let alpha = sin / (2.0 * q.max(0.01));
        let a = db_to_lin(gain_db / 2.0);
        let sqa2 = 2.0 * libm::sqrt(a) * alpha;

        let (b0, b1, b2, a0, a1, a2) = match shape {
            Shape::LowPass => {
                let b = (1.0 - cos) / 2.0;
                (b, 1.0 - cos, b, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            Shape::HighPass => {
                let b = (1.0 + cos) / 2.0;
                (b, -(1.0 + cos), b, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            Shape::Peaking => unreachable!(),
            Shape::LowShelf => (
                a * ((a + 1.0) - (a - 1.0) * cos + sqa2),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - sqa2),
                (a + 1.0) + (a - 1.0) * cos + sqa2,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - sqa2,
            ),
            Shape::HighShelf => (
                a * ((a + 1.0) + (a - 1.0) * cos + sqa2),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - sqa2),
                (a + 1.0) - (a - 1.0) * cos + sqa2,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - sqa2,
            ),
        };

        Coefs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    /// Magnitude response in dB at `freq`.
    pub fn response_db(&self, sample_rate: f64, freq: f64) -> f64 {
        self.response_db_at(&Probe::new(sample_rate, freq))
    }

    /// Magnitude response in dB at a precomputed frequency.
    pub fn response_db_at(&self, p: &Probe) -> f64 {
        let nr = self.b0 + self.b1 * p.c1 + self.b2 * p.c2;
        let ni = -(self.b1 * p.s1 + self.b2 * p.s2);
        let dr = 1.0 + self.a1 * p.c1 + self.a2 * p.c2;
        let di = -(self.a1 * p.s1 + self.a2 * p.s2);
        lin_to_db(libm::sqrt((nr * nr + ni * ni) / (dr * dr + di * di)))
    }
}

/// `cos`/`sin` of `w` and `2w` for one frequency, so responses at a fixed
/// frequency can be evaluated repeatedly without trigonometry.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    c1: f64,
    s1: f64,
    c2: f64,
    s2: f64,
}

impl Probe {
    pub fn new(sample_rate: f64, freq: f64) -> Self {
        let w = 2.0 * PI * freq / sample_rate;
        Self { c1: libm::cos(w), s1: libm::sin(w), c2: libm::cos(2.0 * w), s2: libm::sin(2.0 * w) }
    }
}

/// Peaking filter whose magnitude follows the analog prototype
/// `(s² + s·A/Q + 1) / (s² + s/(A·Q) + 1)` up to Nyquist, instead of being
/// squeezed towards it like the bilinear (cookbook) design. M. Vicanek,
/// "Matched Second Order Digital Filters" (2016): impulse-invariant poles,
/// zeros chosen so the response matches at DC and has its extremum of
/// exactly `gain_db` at `w0` (rad/sample).
fn matched_peaking(w0: f64, q: f64, gain_db: f64) -> Coefs {
    if gain_db < 0.0 {
        // A cut is the inverse of the boost, which keeps boost and cut
        // symmetric. The boost's zeros are minimum phase, so this is stable.
        let c = matched_peaking(w0, q, -gain_db);
        return Coefs { b0: 1.0 / c.b0, b1: c.a1 / c.b0, b2: c.a2 / c.b0, a1: c.b1 / c.b0, a2: c.b2 / c.b0 };
    }
    let g = db_to_lin(gain_db);
    let zeta = 1.0 / (2.0 * q * libm::sqrt(g));
    let r = libm::exp(-zeta * w0);
    let a1 = if zeta <= 1.0 {
        -2.0 * r * libm::cos(w0 * libm::sqrt(1.0 - zeta * zeta))
    } else {
        -2.0 * r * libm::cosh(w0 * libm::sqrt(zeta * zeta - 1.0))
    };
    let a2 = r * r;

    // |H(e^jw)|² written in phi = sin²(w/2):
    // (X0·(1-phi) + X1·phi + X2·4·phi·(1-phi)) for numerator and denominator.
    let aa0 = (1.0 + a1 + a2) * (1.0 + a1 + a2);
    let aa1 = (1.0 - a1 + a2) * (1.0 - a1 + a2);
    let aa2 = -4.0 * a2;
    let s = libm::sin(w0 / 2.0);
    let phi1 = s * s;
    let phi0 = 1.0 - phi1;
    let g2 = g * g;
    let r1 = (aa0 * phi0 + aa1 * phi1 + aa2 * 4.0 * phi0 * phi1) * g2;
    let r2 = (-aa0 + aa1 + 4.0 * (phi0 - phi1) * aa2) * g2;
    let bb0 = aa0;
    let bb2 = (r1 - r2 * phi1 - bb0) / (4.0 * phi1 * phi1);
    let bb1 = r2 + bb0 + 4.0 * (phi1 - phi0) * bb2;

    // Minimum phase factorization of the numerator.
    let (sq0, sq1) = (libm::sqrt(bb0), libm::sqrt(bb1.max(0.0)));
    let w = 0.5 * (sq0 + sq1);
    let b0 = 0.5 * (w + libm::sqrt((w * w + bb2).max(0.0)));
    Coefs { b0, b1: 0.5 * (sq0 - sq1), b2: -bb2 / (4.0 * b0), a1, a2 }
}

/// Transposed direct form II state.
#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    z1: f64,
    z2: f64,
}

impl State {
    #[inline]
    pub fn tick(&mut self, c: &Coefs, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = flush(c.b1 * x - c.a1 * y + self.z2);
        self.z2 = flush(c.b2 * x - c.a2 * y);
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaking_gain_at_center() {
        let c = Coefs::new(Shape::Peaking, 48000.0, 1000.0, 1.0, 6.0);
        assert!((c.response_db(48000.0, 1000.0) - 6.0).abs() < 1e-6);
    }

    /// Analog prototype magnitude in dB.
    fn analog_peaking_db(f0: f64, q: f64, gain_db: f64, f: f64) -> f64 {
        let a = libm::pow(10.0, gain_db / 40.0);
        let x = f / f0;
        let re = (1.0 - x * x) * (1.0 - x * x);
        let num = re + (x * a / q) * (x * a / q);
        let den = re + (x / (a * q)) * (x / (a * q));
        10.0 * libm::log10(num / den)
    }

    #[test]
    fn peaking_matches_analog_near_nyquist() {
        let fs = 48000.0;
        for (f0, q, g) in [(16000.0, 2.87, 12.0), (12000.0, 0.7, -9.0), (20000.0, 1.0, 6.0), (100.0, 4.0, -18.0)] {
            let c = Coefs::new(Shape::Peaking, fs, f0, q, g);
            assert!((c.response_db(fs, f0) - g).abs() < 1e-6, "center {f0}");
            for f in [f0 * 0.5, f0 * 0.71, f0 * 0.9, f0 * 1.1] {
                if f >= fs / 2.0 { continue; }
                let err = c.response_db(fs, f) - analog_peaking_db(f0, q, g, f);
                assert!(err.abs() < 0.6, "f0 {f0} q {q} g {g} at {f}: {err}");
            }
        }
    }

    #[test]
    fn peaking_boost_and_cut_cancel() {
        for (f0, q) in [(50.0, 0.3), (1000.0, 1.0), (15000.0, 8.0)] {
            let up = Coefs::new(Shape::Peaking, 44100.0, f0, q, 7.0);
            let down = Coefs::new(Shape::Peaking, 44100.0, f0, q, -7.0);
            for f in [20.0, 300.0, 3000.0, 12000.0, 21000.0] {
                let sum = up.response_db(44100.0, f) + down.response_db(44100.0, f);
                assert!(sum.abs() < 1e-6, "{f0} {q} at {f}: {sum}");
                assert!(up.response_db(44100.0, f).is_finite());
            }
        }
    }

    #[test]
    fn highpass_is_3db_down_at_cutoff() {
        let c = Coefs::new(Shape::HighPass, 48000.0, 100.0, core::f64::consts::FRAC_1_SQRT_2, 0.0);
        assert!((c.response_db(48000.0, 100.0) + 3.0103).abs() < 0.01);
        assert!(c.response_db(48000.0, 10000.0).abs() < 0.01);
    }
}
