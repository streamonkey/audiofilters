//! Generic element implementation shared by all filters. The
//! `filter_element!` macro creates the GObject subclass boilerplate per
//! filter and forwards everything to [`FilterImp`].

use std::sync::{LazyLock, Mutex};

use byte_slice_cast::AsMutSliceOf;
use gst::glib;
use gst::prelude::*;
use gst_base::prelude::*;
use smkaf_core::{Filter, FilterDesc, ParamKind};

pub static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new("smkaf", gst::DebugColorFlags::empty(), Some("smkaf audio filters"))
});

pub static CAPS: LazyLock<gst::Caps> = LazyLock::new(|| {
    gst_audio::AudioCapsBuilder::new_interleaved()
        .format(gst_audio::AUDIO_FORMAT_F32)
        .build()
});

struct Settings {
    values: Vec<f32>,
    meter: f32,
}

struct State {
    filter: Box<dyn Filter>,
    /// Parameter values last passed to the filter.
    applied: Vec<f32>,
    rate: u32,
    channels: usize,
    latency: usize,
    /// Output frames still to drop: the silence the look-ahead emits before
    /// the first input frame comes out.
    skip: usize,
    /// Whether audio went in since start or the last flush.
    fed: bool,
    /// End time of the last processed input buffer, the timestamp for draining.
    next_pts: Option<gst::ClockTime>,
}

impl State {
    fn time(&self, frames: usize) -> gst::ClockTime {
        gst::ClockTime::SECOND.mul_div_floor(frames as u64, self.rate as u64).unwrap()
    }

    /// Like scaletempo: drop the start-up silence and stamp the buffer with
    /// the time of the audio it holds, i.e. the input time minus the latency.
    /// Returns false if no audio is left.
    fn trim_and_stamp(&mut self, buf: &mut gst::BufferRef, in_pts: Option<gst::ClockTime>) -> bool {
        let bytes_per_frame = self.channels * 4;
        let frames = buf.size() / bytes_per_frame;
        let skip = self.skip.min(frames);
        self.skip -= skip;
        if skip == frames {
            return false;
        }
        if skip > 0 {
            buf.resize(skip * bytes_per_frame..).expect("range within buffer");
        }
        buf.set_pts(in_pts.map(|pts| pts.saturating_sub(self.time(self.latency - skip))));
        buf.set_duration(self.time(frames - skip));
        true
    }
}

pub struct FilterImp {
    desc: &'static FilterDesc,
    settings: Mutex<Settings>,
    state: Mutex<Option<State>>,
}

pub fn properties(desc: &'static FilterDesc) -> Vec<glib::ParamSpec> {
    let mut props: Vec<glib::ParamSpec> = desc
        .params
        .iter()
        .map(|p| {
            let blurb = if p.unit.is_empty() { p.label.to_string() } else { format!("{} ({})", p.label, p.unit) };
            match p.kind {
                ParamKind::Float => glib::ParamSpecFloat::builder(p.name)
                    .nick(p.label)
                    .blurb(&blurb)
                    .minimum(p.min)
                    .maximum(p.max)
                    .default_value(p.default)
                    .mutable_playing()
                    .controllable()
                    .build(),
                ParamKind::Bool => glib::ParamSpecBoolean::builder(p.name)
                    .nick(p.label)
                    .blurb(&blurb)
                    .default_value(p.default != 0.0)
                    .mutable_playing()
                    .controllable()
                    .build(),
                ParamKind::Choice(options) => glib::ParamSpecString::builder(p.name)
                    .nick(p.label)
                    .blurb(&format!("{} ({})", p.label, options.join(", ")))
                    .default_value(Some(options[p.default as usize]))
                    .mutable_playing()
                    .build(),
            }
        })
        .collect();

    if let Some((name, unit)) = desc.meter {
        props.push(
            glib::ParamSpecFloat::builder(name)
                .blurb(&format!("Meter, updated per buffer ({unit})"))
                .minimum(f32::MIN)
                .maximum(f32::MAX)
                .default_value(0.0)
                .read_only()
                .build(),
        );
    }
    props
}

impl FilterImp {
    pub fn new(desc: &'static FilterDesc) -> Self {
        Self {
            desc,
            settings: Mutex::new(Settings { values: desc.params.iter().map(|p| p.default).collect(), meter: 0.0 }),
            state: Mutex::new(None),
        }
    }

    pub fn metadata(desc: &'static FilterDesc) -> gst::subclass::ElementMetadata {
        gst::subclass::ElementMetadata::new(
            &format!("smkaf {}", desc.label),
            "Filter/Effect/Audio",
            desc.description,
            "streaMonkey GmbH",
        )
    }

    pub fn pad_templates() -> Vec<gst::PadTemplate> {
        vec![
            gst::PadTemplate::new("src", gst::PadDirection::Src, gst::PadPresence::Always, &CAPS).unwrap(),
            gst::PadTemplate::new("sink", gst::PadDirection::Sink, gst::PadPresence::Always, &CAPS).unwrap(),
        ]
    }

    /// `id` is GLib's 1-based property id; the properties are the filter
    /// parameters in order, followed by the meter.
    pub fn set_property(&self, obj: &gst::Element, id: usize, value: &glib::Value) {
        let index = id - 1;
        let info = &self.desc.params[index];
        let v = match info.kind {
            ParamKind::Float => value.get::<f32>().unwrap(),
            ParamKind::Bool => value.get::<bool>().unwrap() as u8 as f32,
            ParamKind::Choice(options) => {
                let s = value.get::<Option<String>>().unwrap();
                match s.as_deref().and_then(|s| options.iter().position(|o| *o == s)) {
                    Some(i) => i as f32,
                    None => {
                        gst::warning!(CAT, obj = obj, "invalid {}: {:?}, expected one of {:?}", info.name, s, options);
                        return;
                    }
                }
            }
        };
        gst::debug!(CAT, obj = obj, "{} = {}", info.name, v);
        self.settings.lock().unwrap().values[index] = info.sanitize(v);
    }

    pub fn property(&self, id: usize) -> glib::Value {
        let settings = self.settings.lock().unwrap();
        let index = id - 1;
        if index == self.desc.params.len() {
            return settings.meter.to_value();
        }
        let v = settings.values[index];
        match self.desc.params[index].kind {
            ParamKind::Float => v.to_value(),
            ParamKind::Bool => (v != 0.0).to_value(),
            ParamKind::Choice(options) => options[v as usize].to_value(),
        }
    }

    pub fn setup(&self, obj: &gst_base::BaseTransform, info: &gst_audio::AudioInfo) -> Result<(), gst::LoggableError> {
        let mut filter = smkaf_core::create(self.desc, info.rate() as f32, info.channels() as usize);
        let applied = self.settings.lock().unwrap().values.clone();
        for (i, v) in applied.iter().enumerate() {
            filter.set_param(i, *v);
        }
        filter.reset();
        let latency = filter.latency();
        *self.state.lock().unwrap() = Some(State {
            filter,
            applied,
            rate: info.rate(),
            channels: info.channels() as usize,
            latency,
            skip: latency,
            fed: false,
            next_pts: None,
        });
        if latency > 0 {
            let _ = obj.post_message(gst::message::Latency::builder().src(obj).build());
        }
        Ok(())
    }

    pub fn stop(&self) {
        *self.state.lock().unwrap() = None;
    }

    /// Drop delay lines and envelopes, e.g. after a seek.
    pub fn reset(&self) {
        if let Some(s) = &mut *self.state.lock().unwrap() {
            s.filter.reset();
            s.skip = s.latency;
            s.fed = false;
            s.next_pts = None;
        }
    }

    /// At EOS, push out the audio still held in the filter's look-ahead.
    pub fn drain(&self, obj: &gst_base::BaseTransform) {
        let buf = {
            let mut guard = self.state.lock().unwrap();
            let Some(s) = guard.as_mut().filter(|s| s.latency > 0 && s.fed) else { return };
            let mut buf = gst::Buffer::with_size(s.latency * s.channels * 4).unwrap();
            let buf_mut = buf.get_mut().unwrap();
            {
                let mut map = buf_mut.map_writable().unwrap();
                let samples = map.as_mut_slice_of::<f32>().unwrap();
                samples.fill(0.0);
                s.filter.process(samples);
            }
            if !s.trim_and_stamp(buf_mut, s.next_pts) {
                return;
            }
            buf
        };
        let _ = obj.src_pad().push(buf);
    }

    /// Sync controlled properties to the buffer's stream time.
    pub fn before_transform(&self, obj: &gst_base::BaseTransform, buf: &gst::BufferRef) {
        let segment = obj.segment();
        let stream_time = segment
            .downcast_ref::<gst::ClockTime>()
            .zip(buf.pts())
            .and_then(|(seg, pts)| seg.to_stream_time(pts));
        if let Some(t) = stream_time {
            let _ = obj.sync_values(t);
        }
    }

    pub fn transform_ip(
        &self,
        obj: &gst_base::BaseTransform,
        buf: &mut gst::BufferRef,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        let buf_pts = buf.pts();
        let mut guard = self.state.lock().unwrap();
        let state = guard.as_mut().ok_or(gst::FlowError::NotNegotiated)?;

        {
            let settings = self.settings.lock().unwrap();
            for (i, (&v, applied)) in settings.values.iter().zip(state.applied.iter_mut()).enumerate() {
                if v != *applied {
                    state.filter.set_param(i, v);
                    *applied = v;
                }
            }
        }
        // A new latency means the filter restarted its delay line.
        let latency = state.filter.latency();
        let latency_changed = latency != state.latency;
        if latency_changed {
            state.latency = latency;
            state.skip = latency;
        }

        let frames = {
            let mut map = buf.map_writable().map_err(|_| {
                gst::element_error!(obj, gst::CoreError::Failed, ["Failed to map buffer writable"]);
                gst::FlowError::Error
            })?;
            let samples = map.as_mut_slice_of::<f32>().map_err(|_| {
                gst::element_error!(obj, gst::CoreError::Failed, ["Buffer is not a multiple of f32"]);
                gst::FlowError::Error
            })?;
            state.filter.process(samples);
            samples.len() / state.channels
        };
        state.fed = true;
        state.next_pts = buf_pts.map(|pts| pts + state.time(frames));
        let keep = state.trim_and_stamp(buf, buf_pts);

        let meter = state.filter.meter();
        drop(guard);

        self.settings.lock().unwrap().meter = meter;
        if latency_changed {
            gst::info!(CAT, obj = obj, "latency changed to {} frames", latency);
            let _ = obj.post_message(gst::message::Latency::builder().src(obj).build());
        }
        Ok(if keep { gst::FlowSuccess::Ok } else { gst_base::BASE_TRANSFORM_FLOW_DROPPED })
    }

    /// Our processing latency, if negotiated.
    pub fn latency(&self) -> gst::ClockTime {
        self.state.lock().unwrap().as_ref().map_or(gst::ClockTime::ZERO, |s| s.time(s.latency))
    }
}

/// Define one element module per filter, plus `plugin_init` registering
/// them all as `smkaf<filter name>`.
macro_rules! filter_elements {
    ($($module:ident: $type_name:literal => $desc:path;)*) => {
        $(filter_element!($module, $type_name, $desc);)*

        fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
            $($module::register(plugin)?;)*
            Ok(())
        }
    };
}

macro_rules! filter_element {
    ($module:ident, $type_name:literal, $desc:path) => {
        pub mod $module {
            use gst::glib;
            use gst::prelude::*;

            glib::wrapper! {
                pub struct Element(ObjectSubclass<imp::Imp>)
                    @extends gst_audio::AudioFilter, gst_base::BaseTransform, gst::Element, gst::Object;
            }

            pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
                let name = format!("smkaf{}", $desc.name);
                gst::Element::register(Some(plugin), &name, gst::Rank::NONE, Element::static_type())
            }

            mod imp {
                use std::sync::LazyLock;

                use gst::glib;
                use gst::prelude::*;
                use gst::subclass::prelude::*;
                use gst_audio::subclass::prelude::*;

                use crate::element::FilterImp;

                pub struct Imp(FilterImp);

                impl Default for Imp {
                    fn default() -> Self {
                        Self(FilterImp::new(&$desc))
                    }
                }

                #[glib::object_subclass]
                impl ObjectSubclass for Imp {
                    const NAME: &'static str = $type_name;
                    type Type = super::Element;
                    type ParentType = gst_audio::AudioFilter;
                }

                impl ObjectImpl for Imp {
                    fn properties() -> &'static [glib::ParamSpec] {
                        static PROPS: LazyLock<Vec<glib::ParamSpec>> =
                            LazyLock::new(|| crate::element::properties(&$desc));
                        PROPS.as_ref()
                    }

                    fn set_property(&self, id: usize, value: &glib::Value, _pspec: &glib::ParamSpec) {
                        self.0.set_property(self.obj().upcast_ref(), id, value)
                    }

                    fn property(&self, id: usize, _pspec: &glib::ParamSpec) -> glib::Value {
                        self.0.property(id)
                    }
                }

                impl GstObjectImpl for Imp {}

                impl ElementImpl for Imp {
                    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
                        static META: LazyLock<gst::subclass::ElementMetadata> =
                            LazyLock::new(|| FilterImp::metadata(&$desc));
                        Some(&*META)
                    }

                    fn pad_templates() -> &'static [gst::PadTemplate] {
                        static TEMPLATES: LazyLock<Vec<gst::PadTemplate>> =
                            LazyLock::new(FilterImp::pad_templates);
                        TEMPLATES.as_ref()
                    }
                }

                impl BaseTransformImpl for Imp {
                    const MODE: gst_base::subclass::BaseTransformMode =
                        gst_base::subclass::BaseTransformMode::AlwaysInPlace;
                    const PASSTHROUGH_ON_SAME_CAPS: bool = false;
                    const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;

                    fn stop(&self) -> Result<(), gst::ErrorMessage> {
                        self.0.stop();
                        Ok(())
                    }

                    fn sink_event(&self, event: gst::Event) -> bool {
                        match event.view() {
                            gst::EventView::FlushStop(_) => self.0.reset(),
                            gst::EventView::Eos(_) => self.0.drain(self.obj().upcast_ref()),
                            _ => {}
                        }
                        self.parent_sink_event(event)
                    }

                    fn before_transform(&self, buf: &gst::BufferRef) {
                        self.0.before_transform(self.obj().upcast_ref(), buf)
                    }

                    fn transform_ip(&self, buf: &mut gst::BufferRef) -> Result<gst::FlowSuccess, gst::FlowError> {
                        self.0.transform_ip(self.obj().upcast_ref(), buf)
                    }

                    fn query(&self, direction: gst::PadDirection, query: &mut gst::QueryRef) -> bool {
                        let ret = BaseTransformImplExt::parent_query(self, direction, query);
                        if ret && direction == gst::PadDirection::Src {
                            if let gst::QueryViewMut::Latency(q) = query.view_mut() {
                                let ours = self.0.latency();
                                let (live, min, max) = q.result();
                                q.set(live, min + ours, max.map(|m| m + ours));
                            }
                        }
                        ret
                    }
                }

                impl AudioFilterImpl for Imp {
                    fn allowed_caps() -> &'static gst::Caps {
                        &crate::element::CAPS
                    }

                    fn setup(&self, info: &gst_audio::AudioInfo) -> Result<(), gst::LoggableError> {
                        self.parent_setup(info)?;
                        self.0.setup(self.obj().upcast_ref(), info)
                    }
                }
            }
        }
    };
}
