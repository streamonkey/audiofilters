//! Raw `extern "C"` ABI around `smkaf-core` for use inside an
//! AudioWorkletGlobalScope. No wasm-bindgen: the module has no imports, so it
//! can be instantiated in the worklet without `fetch` or `TextDecoder`.
//!
//! Audio is exchanged through an interleaved `f32` buffer that lives inside
//! the wasm memory (see [`smkaf_buffer`]). Handles are opaque pointers;
//! `0` means failure.

use smkaf_core::{Filter, FILTERS};

/// Maximum number of frames per [`smkaf_process`] call.
pub const MAX_FRAMES: usize = 128;

pub struct Instance {
    filter: Box<dyn Filter>,
    channels: usize,
    buffer: Vec<f32>,
}

#[no_mangle]
pub extern "C" fn smkaf_max_frames() -> u32 {
    MAX_FRAMES as u32
}

/// Create a filter of `kind` with all parameters at their defaults.
#[no_mangle]
pub extern "C" fn smkaf_new(kind: u32, sample_rate: f32, channels: u32) -> *mut Instance {
    let Some(desc) = FILTERS.get(kind as usize) else { return std::ptr::null_mut() };
    if channels == 0 || !(sample_rate > 0.0) {
        return std::ptr::null_mut();
    }
    let channels = channels as usize;
    Box::into_raw(Box::new(Instance {
        filter: smkaf_core::create(desc, sample_rate, channels),
        channels,
        buffer: vec![0.0; MAX_FRAMES * channels],
    }))
}

/// # Safety
/// `h` must come from [`smkaf_new`] and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn smkaf_free(h: *mut Instance) {
    if !h.is_null() {
        drop(Box::from_raw(h));
    }
}

/// Pointer to the interleaved I/O buffer of `MAX_FRAMES * channels` floats.
/// Stays valid for the lifetime of the instance, but JS views on it must be
/// recreated whenever `memory.buffer` changes.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn smkaf_buffer(h: *mut Instance) -> *mut f32 {
    (*h).buffer.as_mut_ptr()
}

/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn smkaf_set_param(h: *mut Instance, index: u32, value: f32) {
    (*h).filter.set_param(index as usize, value);
}

/// Process `frames` (<= `MAX_FRAMES`) frames of the I/O buffer in place.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn smkaf_process(h: *mut Instance, frames: u32) {
    let inst = &mut *h;
    let n = (frames as usize).min(MAX_FRAMES) * inst.channels;
    inst.filter.process(&mut inst.buffer[..n]);
}

/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn smkaf_reset(h: *mut Instance) {
    (*h).filter.reset();
}

/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn smkaf_latency(h: *mut Instance) -> u32 {
    (*h).filter.latency() as u32
}

/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn smkaf_meter(h: *mut Instance) -> f32 {
    (*h).filter.meter()
}

/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn smkaf_graph(h: *mut Instance, x: f32) -> f32 {
    (*h).filter.graph(x)
}
