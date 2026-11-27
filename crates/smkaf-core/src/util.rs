//! Small math helpers shared by the filters. Everything is `f64` internally;
//! audio enters and leaves as `f32`.

/// Smallest level considered by the detectors (-240 dBFS).
const MIN_LEVEL: f64 = 1e-12;

#[inline]
pub fn db_to_lin(db: f64) -> f64 {
    libm::exp(db * (core::f64::consts::LN_10 / 20.0))
}

/// [`db_to_lin`] that skips the `exp` while the input stays the same, which
/// is the common steady state of gain envelopes (gate open or closed,
/// compressor idle, leveler frozen). Bit-identical to calling `db_to_lin`.
#[derive(Debug, Clone, Copy)]
pub struct DbToLin {
    db: f64,
    lin: f64,
}

impl DbToLin {
    pub const fn new() -> Self {
        Self { db: 0.0, lin: 1.0 }
    }

    #[inline]
    pub fn get(&mut self, db: f64) -> f64 {
        if db != self.db {
            self.db = db;
            self.lin = db_to_lin(db);
        }
        self.lin
    }
}

/// Amplitude to dBFS.
#[inline]
pub fn lin_to_db(x: f64) -> f64 {
    20.0 * libm::log10(if x > MIN_LEVEL { x } else { MIN_LEVEL })
}

/// Power (mean square) to dBFS.
#[inline]
pub fn pow_to_db(x: f64) -> f64 {
    10.0 * libm::log10(if x > MIN_LEVEL * MIN_LEVEL { x } else { MIN_LEVEL * MIN_LEVEL })
}

/// Largest absolute sample value of an interleaved frame.
#[inline]
pub fn peak(frame: &[f32]) -> f64 {
    let mut peak = 0.0f64;
    for s in frame {
        let a = libm::fabs(*s as f64);
        if a > peak {
            peak = a;
        }
    }
    peak
}

/// Mean square over all samples of an interleaved frame.
#[inline]
pub fn mean_square(frame: &[f32]) -> f64 {
    let mut sum = 0.0f64;
    for s in frame {
        let x = *s as f64;
        sum += x * x;
    }
    sum / frame.len() as f64
}

/// One-pole coefficient for a time constant in milliseconds.
#[inline]
pub fn time_coef(ms: f64, sample_rate: f64) -> f64 {
    if ms <= 0.0 {
        0.0
    } else {
        libm::exp(-1000.0 / (ms * sample_rate))
    }
}

/// Flush values that are about to become denormal, and NaN/Inf, to zero, so
/// recursive state recovers from a bad input sample.
#[inline]
pub fn flush(x: f64) -> f64 {
    if libm::fabs(x) >= 1e-30 && x.is_finite() {
        x
    } else {
        0.0
    }
}

/// One-pole parameter smoother.
#[derive(Debug, Clone, Copy)]
pub struct Smoother {
    current: f64,
    target: f64,
    coef: f64,
}

/// Filters that derive expensive state (e.g. biquad coefficients) from
/// smoothed parameters update it once per this many frames, counted inside
/// the filter so the caller's block size does not matter.
pub const CONTROL_FRAMES: u32 = 16;

/// Default parameter smoothing time.
pub const SMOOTH_MS: f64 = 20.0;

impl Smoother {
    pub fn new(value: f64, sample_rate: f64) -> Self {
        Self { current: value, target: value, coef: time_coef(SMOOTH_MS, sample_rate) }
    }

    pub fn set(&mut self, target: f64) {
        self.target = target;
    }

    pub fn target(&self) -> f64 {
        self.target
    }

    pub fn is_settled(&self) -> bool {
        self.current == self.target
    }

    /// Jump to the target immediately.
    pub fn snap(&mut self) {
        self.current = self.target;
    }

    #[inline]
    pub fn next(&mut self) -> f64 {
        if self.current != self.target {
            self.current = self.target + (self.current - self.target) * self.coef;
            if libm::fabs(self.current - self.target) < 1e-9 {
                self.current = self.target;
            }
        }
        self.current
    }
}

/// Attack/release smoothing of a gain value in dB. `attack` is used while
/// the gain moves down (more reduction), `release` while it moves up.
#[inline]
pub fn ballistics(state: f64, target: f64, attack: f64, release: f64) -> f64 {
    let c = if target < state { attack } else { release };
    flush(target + (state - target) * c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_roundtrip() {
        for db in [-60.0, -6.0, 0.0, 12.0] {
            assert!((lin_to_db(db_to_lin(db)) - db).abs() < 1e-9);
        }
    }

    #[test]
    fn smoother_settles_exactly() {
        let mut s = Smoother::new(0.0, 48000.0);
        s.set(1.0);
        for _ in 0..48000 {
            s.next();
        }
        assert!(s.is_settled());
        assert_eq!(s.next(), 1.0);
    }
}
