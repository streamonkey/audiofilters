//! GStreamer plugin `smkaf`: one `AudioFilter` element per filter of
//! `smkaf-core`, named `smkaf<filter>` (e.g. `smkafcompressor`). Element
//! properties are generated from the filter's parameter descriptors.

use gst::glib;

#[macro_use]
mod element;

filter_elements! {
    compressor: "SmkafCompressor" => smkaf_core::compressor::DESC;
    limiter: "SmkafLimiter" => smkaf_core::limiter::DESC;
    gate: "SmkafGate" => smkaf_core::gate::DESC;
    eq: "SmkafEq" => smkaf_core::eq::DESC;
    graphiceq: "SmkafGraphicEq" => smkaf_core::graphiceq::DESC;
    deesser: "SmkafDeesser" => smkaf_core::deesser::DESC;
    leveler: "SmkafLeveler" => smkaf_core::leveler::DESC;
    stereo: "SmkafStereo" => smkaf_core::stereo::DESC;
}

gst::plugin_define!(
    smkaf,
    env!("CARGO_PKG_DESCRIPTION"),
    plugin_init,
    concat!(env!("CARGO_PKG_VERSION"), "-", env!("COMMIT_ID")),
    "MIT/X11",
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_REPOSITORY"),
    env!("BUILD_REL_DATE")
);
