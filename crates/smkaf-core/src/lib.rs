//! Platform independent DSP core of the smkaf audio filter collection.
//!
//! Every filter lives in its own module and is described by a static
//! [`FilterDesc`]. The descriptors are the single source of truth for the
//! wrappers: the GStreamer plugin derives its elements and properties from
//! them, the WASM module dispatches on their index, and the npm package's
//! TypeScript metadata is generated from them.
//!
//! Rules for code in this crate (they keep native and wasm output identical):
//! - all transcendental math goes through `libm`, never `std`/intrinsics
//! - no SIMD, no fused multiply-add, no fast-math
//! - parameter smoothing happens per sample inside the filters, so the rate
//!   at which a wrapper calls [`Filter::set_param`] does not matter
//! - recursive state is flushed to zero before it can become denormal
#![no_std]

extern crate alloc;

use alloc::boxed::Box;

pub mod biquad;
pub mod compressor;
pub mod deesser;
pub mod eq;
pub mod gate;
pub mod graphiceq;
pub mod leveler;
pub mod limiter;
pub mod stereo;
pub mod util;

/// Type of a parameter. All parameter values are transported as `f32`;
/// booleans are `0.0`/`1.0` and choices are the index into the option list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamKind {
    Float,
    Bool,
    Choice(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy)]
pub struct ParamInfo {
    /// Machine name, kebab-case. Used as GStreamer property name and as
    /// AudioParam name in the browser.
    pub name: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub kind: ParamKind,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

impl ParamInfo {
    pub const fn float(
        name: &'static str,
        label: &'static str,
        unit: &'static str,
        min: f32,
        max: f32,
        default: f32,
    ) -> Self {
        Self { name, label, unit, kind: ParamKind::Float, min, max, default }
    }

    pub const fn toggle(name: &'static str, label: &'static str, default: bool) -> Self {
        Self {
            name,
            label,
            unit: "",
            kind: ParamKind::Bool,
            min: 0.0,
            max: 1.0,
            default: if default { 1.0 } else { 0.0 },
        }
    }

    pub const fn choice(
        name: &'static str,
        label: &'static str,
        options: &'static [&'static str],
        default: usize,
    ) -> Self {
        Self {
            name,
            label,
            unit: "",
            kind: ParamKind::Choice(options),
            min: 0.0,
            max: (options.len() - 1) as f32,
            default: default as f32,
        }
    }

    /// Clamp to the valid range, replace NaN by the default and round
    /// booleans/choices to an integer.
    pub fn sanitize(&self, v: f32) -> f32 {
        let v = if v.is_nan() { self.default } else { v.clamp(self.min, self.max) };
        match self.kind {
            ParamKind::Float => v,
            ParamKind::Bool | ParamKind::Choice(_) => libm::roundf(v),
        }
    }
}

/// What [`Filter::graph`] computes, so UIs know how to draw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphKind {
    /// No graph.
    None,
    /// Static transfer curve: input level in dBFS -> output level in dBFS.
    Transfer,
    /// Magnitude response: frequency in Hz -> gain in dB.
    Frequency,
}

/// Static description of a filter type.
pub struct FilterDesc {
    /// Machine name, e.g. `compressor`. The GStreamer element is called
    /// `smkaf<name>`.
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub params: &'static [ParamInfo],
    pub graph: GraphKind,
    /// Name and unit of the value returned by [`Filter::meter`], if any.
    pub meter: Option<(&'static str, &'static str)>,
    /// Construct with placeholder parameters; use [`create`], which applies
    /// the defaults.
    pub new: fn(sample_rate: f32, channels: usize) -> Box<dyn Filter>,
}

/// A running filter instance for a fixed sample rate and channel count.
pub trait Filter: Send {
    /// Set parameter `index` (into [`FilterDesc::params`]). Values are
    /// sanitized, out-of-range indices are ignored. Changes are smoothed
    /// internally where that makes sense.
    fn set_param(&mut self, index: usize, value: f32);

    /// Process interleaved audio in place. `buf.len()` must be a multiple of
    /// the channel count.
    fn process(&mut self, buf: &mut [f32]);

    /// Clear all signal state (delay lines, envelopes) without touching
    /// parameters.
    fn reset(&mut self);

    /// Processing latency in frames.
    fn latency(&self) -> usize {
        0
    }

    /// Current meter value, see [`FilterDesc::meter`].
    fn meter(&self) -> f32 {
        0.0
    }

    /// Evaluate the graph described by [`FilterDesc::graph`] at `x`, using
    /// the current target parameter values.
    fn graph(&self, _x: f32) -> f32 {
        0.0
    }
}

/// All filters. The index is the filter "kind" of the WASM ABI; the npm
/// build generates its metadata from this list, so wasm and JS always agree.
pub static FILTERS: &[&FilterDesc] = &[
    &compressor::DESC,
    &limiter::DESC,
    &gate::DESC,
    &eq::DESC,
    &graphiceq::DESC,
    &deesser::DESC,
    &leveler::DESC,
    &stereo::DESC,
];

pub fn find(name: &str) -> Option<&'static FilterDesc> {
    FILTERS.iter().copied().find(|d| d.name == name)
}

/// Create a filter and apply all parameter defaults.
pub fn create(desc: &FilterDesc, sample_rate: f32, channels: usize) -> Box<dyn Filter> {
    let mut f = (desc.new)(sample_rate, channels.max(1));
    for (i, p) in desc.params.iter().enumerate() {
        f.set_param(i, p.default);
    }
    f.reset();
    f
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    /// Output must not depend on how the caller splits the audio into blocks
    /// (GStreamer buffers vs. 128-frame worklet quanta).
    #[test]
    fn block_size_independent() {
        let mut seed = 7u32;
        let input: Vec<f32> = (0..2 * 48000)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0;
                noise * if (i / 9600) % 2 == 0 { 0.9 } else { 0.01 }
            })
            .collect();
        for desc in super::FILTERS {
            let run = |block: usize| {
                let mut f = super::create(desc, 48000.0, 2);
                // non-default value for every parameter, so smoothing is exercised
                for (i, p) in desc.params.iter().enumerate() {
                    f.set_param(i, p.min + (p.max - p.min) * 0.3);
                }
                let mut buf = input.clone();
                for chunk in buf.chunks_mut(block * 2) {
                    f.process(chunk);
                }
                buf
            };
            assert!(run(128) == run(1001), "{} depends on block size", desc.name);
        }
    }

    /// A single NaN/Inf input sample must not break a filter permanently.
    #[test]
    fn recovers_from_non_finite_input() {
        for desc in super::FILTERS {
            let mut f = super::create(desc, 48000.0, 2);
            let mut buf = alloc::vec![0.25f32; 2 * 48000];
            buf[100] = f32::NAN;
            buf[101] = f32::INFINITY;
            f.process(&mut buf);
            assert!(buf[48000..].iter().all(|s| s.is_finite()), "{} stays broken", desc.name);
            assert!(f.meter().is_finite(), "{} meter stays broken", desc.name);
        }
    }
}
