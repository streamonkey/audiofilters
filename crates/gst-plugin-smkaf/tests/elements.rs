use byte_slice_cast::{AsMutSliceOf, AsSliceOf};
use gst::prelude::*;

const RATE: u32 = 48000;
const FRAMES: usize = 1024;

fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        gst::init().unwrap();
        gstsmkaf::plugin_register_static().unwrap();
    });
}

fn harness(element: &str, channels: i32) -> gst_check::Harness {
    let mut h = gst_check::Harness::new(element);
    let mut caps = gst_audio::AudioCapsBuilder::new_interleaved()
        .format(gst_audio::AUDIO_FORMAT_F32)
        .rate(RATE as i32)
        .channels(channels);
    if channels > 2 {
        caps = caps.channel_mask(0);
    }
    let caps = caps.build();
    h.set_src_caps(caps);
    h
}

/// Push `blocks` buffers of pseudo-random noise with the given amplitude and
/// return all output samples.
fn push_noise(h: &mut gst_check::Harness, channels: usize, blocks: usize, amplitude: f32) -> Vec<f32> {
    let mut seed = 22u32;
    let mut out = Vec::new();
    for b in 0..blocks {
        let mut buf = gst::Buffer::with_size(FRAMES * channels * 4).unwrap();
        {
            let buf = buf.get_mut().unwrap();
            buf.set_pts(gst::ClockTime::SECOND.mul_div_floor((b * FRAMES) as u64, RATE as u64));
            let mut map = buf.map_writable().unwrap();
            for s in map.as_mut_slice_of::<f32>().unwrap() {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *s = ((seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0) * amplitude;
            }
        }
        let outbuf = h.push_and_pull(buf).unwrap();
        out.extend_from_slice(outbuf.map_readable().unwrap().as_slice_of::<f32>().unwrap());
    }
    out
}

#[test]
fn all_elements_process_audio() {
    init();
    for desc in smkaf_core::FILTERS {
        for channels in [1, 2, 6] {
            let mut h = harness(&format!("smkaf{}", desc.name), channels);
            let out = push_noise(&mut h, channels as usize, 4, 0.5);
            // The look-ahead's start-up silence is dropped.
            let latency = smkaf_core::create(desc, RATE as f32, channels as usize).latency();
            assert_eq!(out.len(), (4 * FRAMES - latency) * channels as usize);
            assert!(out.iter().all(|s| s.is_finite()), "{} produced non-finite samples", desc.name);
        }
    }
}

#[test]
fn limiter_respects_ceiling_and_reports_latency() {
    init();
    let mut h = harness("smkaflimiter", 2);
    let limiter = h.element().unwrap();
    limiter.set_property("input-gain", 12.0f32);
    limiter.set_property("ceiling", -3.0f32);
    let out = push_noise(&mut h, 2, 50, 0.9);
    // The ceiling was changed after negotiation and ramps to -3 dB within
    // the parameter smoothing time, skip that part.
    let out = &out[20 * FRAMES * 2..];
    let ceiling = 10f32.powf(-3.0 / 20.0);
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak <= ceiling * (1.0 + 1e-6), "{peak} > {ceiling}");

    let reduction: f32 = limiter.property("gain-reduction");
    assert!(reduction < -1.0, "meter should show gain reduction, got {reduction}");

    // 5 ms default look-ahead at 48 kHz = 240 frames window -> 239 frames latency.
    h.set_upstream_latency(gst::ClockTime::ZERO);
    let mut q = gst::query::Latency::new();
    assert!(limiter.static_pad("src").unwrap().query(&mut q));
    let (_, min, _) = q.result();
    assert_eq!(min, gst::ClockTime::SECOND.mul_div_floor(239, RATE as u64).unwrap());
}

#[test]
fn choice_properties_roundtrip() {
    init();
    let e = gst::ElementFactory::make("smkafcompressor").build().unwrap();
    assert_eq!(e.property::<String>("detector"), "peak");
    e.set_property("detector", "rms");
    assert_eq!(e.property::<String>("detector"), "rms");
    e.set_property("ratio", 8.0f32);
    assert_eq!(e.property::<f32>("ratio"), 8.0);
}

#[test]
fn flush_clears_limiter_delay_line() {
    init();
    let mut h = harness("smkaflimiter", 1);
    push_noise(&mut h, 1, 4, 0.5);

    assert!(h.push_event(gst::event::FlushStart::new()));
    assert!(h.push_event(gst::event::FlushStop::new(true)));
    let segment = gst::FormattedSegment::<gst::ClockTime>::new();
    assert!(h.push_event(gst::event::Segment::new(&segment)));

    // Silence in must give silence out right away, without pre-seek audio
    // from the look-ahead delay line.
    let out = push_noise(&mut h, 1, 1, 0.0);
    assert!(out.iter().all(|&s| s == 0.0));
}

/// Like scaletempo, the limiter drops its start-up silence and stamps output
/// with the time of the audio it holds; at EOS it drains the look-ahead. So
/// offline, the output is the input, sample-aligned and of the same length.
#[test]
fn limiter_output_is_aligned_with_input() {
    init();
    let mut h = harness("smkaflimiter", 1);
    // Below the ceiling, so the limiter applies unity gain.
    let input: Vec<f32> = (0..3 * FRAMES).map(|i| ((i * 7919) % 1000) as f32 / 4000.0).collect();
    for (b, chunk) in input.chunks(FRAMES).enumerate() {
        let mut buf = gst::Buffer::from_mut_slice(chunk.iter().flat_map(|s| s.to_ne_bytes()).collect::<Vec<u8>>());
        buf.get_mut().unwrap().set_pts(gst::ClockTime::SECOND.mul_div_floor((b * FRAMES) as u64, RATE as u64));
        assert_eq!(h.push(buf), Ok(gst::FlowSuccess::Ok));
    }
    assert!(h.push_event(gst::event::Eos::new()));

    let mut output = Vec::new();
    while let Some(buf) = h.try_pull() {
        let expected_pts = gst::ClockTime::SECOND.mul_div_floor(output.len() as u64, RATE as u64).unwrap();
        let pts = buf.pts().unwrap();
        // Timestamps are integer nanoseconds, so allow rounding.
        assert!(pts.absdiff(expected_pts) <= gst::ClockTime::from_nseconds(1), "{pts} vs {expected_pts}");
        output.extend_from_slice(buf.map_readable().unwrap().as_slice_of::<f32>().unwrap());
    }
    assert_eq!(output, input);
}
