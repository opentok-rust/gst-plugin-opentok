// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use crate::common::{caps, gst_from_otc_format, init, pipe_opentok_to_gst_log, Credentials, Error};

use anyhow::anyhow;
use byte_slice_cast::*;
use gst::glib::subclass::prelude::*;
use gst::glib::{self, clone, ToValue};
use gst::prelude::*;
use gst::subclass::prelude::*;
use once_cell::sync::Lazy;
use opentok::audio_device::{AudioDevice, AudioSample};
use opentok::session::{Session, SessionCallbacks};
use opentok::subscriber::{Subscriber as OpenTokSubscriber, SubscriberCallbacks};
use opentok::video_frame::VideoFrame;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "opentoksrc",
        gst::DebugColorFlags::empty(),
        Some("OpenTok Source"),
    )
});

#[allow(dead_code)]
struct Subscriber {
    subscriber: OpenTokSubscriber,
    video_appsrc: gst::Element,
    video_pad: gst::GhostPad,
}

struct State {
    /// OpenTok session credentials (API key, session ID and token).
    credentials: Credentials,
    /// OpenTok Session instance.
    /// Every Vonage Video API video chat occurs within a session.
    /// You can think of a session as a “room” where clients can interact
    /// with one another in real-time.
    session: Option<Session>,
    /// OpenTok Stream identifier.
    /// We will be connecting to this stream only.
    stream_id: Option<String>,
    /// List of OpenTok subscriber instances.
    /// We create a new subscriber per each stream that is created in the
    /// session, unless `stream_id` is set, in which case, a single subscriber
    /// for that specific stream is created.
    subscribers: HashMap<String, Subscriber>,
    flow_combiner: gst_base::UniqueFlowCombiner,
}

pub struct OpenTokSrc {
    state: Arc<Mutex<State>>,
    /// Pad template for the video stream.
    video_src_pad_template: gst::PadTemplate,
    /// Pad template for the audio stream.
    audio_src_pad_template: gst::PadTemplate,
}

struct AppSrcStateHolder {
    allocator: Option<gst::Allocator>,
    query_allocator: bool,
}

impl AppSrcStateHolder {
    fn push_sample(
        holder: &Arc<Mutex<AppSrcStateHolder>>,
        caps: &gst::Caps,
        src: &gst::Element,
        data: &[u8],
    ) {
        let query_allocator = holder.lock().unwrap().query_allocator;
        if query_allocator && holder.lock().unwrap().allocator.is_none() {
            let mut allocation_query = gst::query::Allocation::new(Some(caps), true);
            let appsrc_pad = src.static_pad("src").unwrap();
            if appsrc_pad.peer_query(&mut allocation_query) {
                let mut allocator_found = false;
                for param in allocation_query.allocation_params() {
                    if let Some(allocator) = param.0 {
                        holder.lock().unwrap().allocator = Some(allocator);
                        allocator_found = true;
                        break;
                    }
                }
                if !allocator_found {
                    holder.lock().unwrap().query_allocator = false;
                }
            } else {
                holder.lock().unwrap().query_allocator = false;
            }
        }

        let mut buffer = match holder.lock().unwrap().allocator {
            Some(ref allocator) => {
                let mem = allocator.alloc(data.len(), None).unwrap();

                let mut writable_mem = mem.into_mapped_memory_writable().unwrap();
                let mem_data = writable_mem.as_mut_slice();
                let bytes = mem_data.as_mut_slice_of::<u8>().unwrap();
                // FIXME: Find a better way to do this.
                bytes.clone_from_slice(data);
                let mem = writable_mem.into_memory();
                let mut buffer = gst::Buffer::new();
                {
                    let buffer = buffer.get_mut().unwrap();
                    buffer.append_memory(mem);
                }
                buffer
            }
            None => gst::Buffer::from_slice(data.to_vec()),
        };

        if let Some(timestamp) = src.current_running_time() {
            let buffer = buffer.get_mut().unwrap();
            buffer.set_pts(Some(timestamp));
            buffer.set_dts(Some(timestamp));
        }

        let sample = gst::Sample::builder().caps(caps).buffer(&buffer).build();
        let appsrc = src
            .clone()
            .dynamic_cast::<gst_app::AppSrc>()
            .expect("Element is expected to be an appsrc!");
        if let Err(err) = appsrc.push_sample(&sample) {
            gst::error!(CAT, obj: &appsrc, "Failed to push sample: {:?}", err);
        };
    }
}

impl State {
    fn set_location(
        &mut self,
        element: &super::OpenTokSrc,
        location: &str,
    ) -> Result<(), glib::BoolError> {
        if self.credentials.room_uri().is_some() {
            return Err(glib::BoolError::new(
                format!("Credential already set {:?}", self.credentials),
                file!(),
                "set_location",
                line!(),
            ));
        }

        gst::debug!(CAT, obj: element, "Setting location to {}", location);
        let url = Url::parse(location).map_err(|err| {
            glib::BoolError::new(
                format!("Malformed url {:?}", err),
                file!(),
                "set_location",
                line!(),
            )
        })?;
        let credentials: Credentials = url.into();
        gst::debug!(CAT, obj: element, "Credentials {:?}", credentials);
        if let Some(ref stream_id) = credentials.stream_id() {
            if !stream_id.is_empty() {
                self.set_stream_id(element, stream_id.to_string());
            }
        }

        self.credentials = credentials;
        Ok(())
    }

    fn set_stream_id(&mut self, element: &super::OpenTokSrc, id: String) {
        gst::debug!(CAT, obj: element, "Setting stream ID to {}", id);
        self.stream_id = Some(id);
    }

    fn stream_received(
        &mut self,
        element: &super::OpenTokSrc,
        video_src_pad_template: &gst::PadTemplate,
        session: &opentok::session::Session,
        stream: opentok::stream::Stream,
    ) -> Result<(gst::Element, gst::GhostPad), Error> {
        let stream_id = stream.id();
        gst::debug!(CAT, obj: element, "Stream received {}", stream_id);

        if let Some(ref stream_id_to_subscribe) = self.stream_id {
            gst::debug!(
                CAT,
                obj: element,
                "We want to subscribe to stream {}",
                stream_id_to_subscribe
            );
            if *stream_id_to_subscribe != stream_id {
                return Err(Error::InvalidStream(
                    stream_id,
                    stream_id_to_subscribe.to_string(),
                ));
            }
        } else {
            gst::debug!(CAT, obj: element, "We want to subscribe to all streams");
        }

        // The stream may grow a video feed at some point during its life time and we
        // won't get any other notification about it, so we make sure to setup the video
        // pipeline even if the stream has no video enabled at this point.

        let bin = element
            .upcast_ref::<gst::Element>()
            .clone()
            .downcast::<gst::Bin>()
            .unwrap();
        gst::debug!(CAT, obj: element, "Setup video stream");
        let video_appsrc = gst::ElementFactory::make("appsrc")
            .build()
            .map_err(|_| Error::MissingElement("appsrc"))?;
        video_appsrc.set_property("is-live", true);
        video_appsrc.set_property("format", gst::Format::Time);

        bin.add(&video_appsrc)
            .map_err(|_| Error::AddElement("appsrc"))?;

        let pad_name = generate_video_pad_name(&self.subscribers);
        let appsrc_src_pad = video_appsrc.static_pad("src").unwrap();

        let templ = &video_src_pad_template;
        let video_pad = gst::GhostPad::builder_with_template(templ, Some(pad_name.as_str()))
            .proxy_pad_chain_function({
                let element_weak = element.downgrade();
                move |pad, _parent, buffer| {
                    let element = match element_weak.upgrade() {
                        None => return Err(gst::FlowError::Flushing),
                        Some(element) => element,
                    };

                    element.imp().proxy_pad_chain(pad, buffer)
                }
            })
            .build_with_target(&appsrc_src_pad)
            .unwrap();

        self.flow_combiner.add_pad(&video_pad);

        let appsrc_state_holder = AppSrcStateHolder {
            allocator: None,
            query_allocator: true,
        };
        let holder = Arc::new(Mutex::new(appsrc_state_holder));

        let subscriber_callbacks = SubscriberCallbacks::builder()
            .on_render_frame(clone!(
                @weak element,
                @weak video_appsrc,
            => move |_, frame| {
                OpenTokSrc::push_video_frame(&holder, &video_appsrc, frame)
            }))
            .on_error(clone!(
                @weak element
            => move |_, error, _| {
                gst::error!(CAT, obj: &element, "Error notified from subscriber: {:?}", error);
            }))
            .on_audio_enabled(clone!(
                @weak element
            => move |_| {
                gst::debug!(CAT, obj: &element, "Audio enabled");
            }))
            .on_audio_disabled(clone!(
                @weak element
            => move |_| {
                gst::debug!(CAT, obj: &element, "Audio disabled");
            }))
            .on_video_enabled(clone!(@weak video_pad,
                                     @weak element,
                                     @weak video_appsrc => move |_, _| {
                element.imp().enable_video(&video_pad, &video_appsrc);
            }))
            .on_video_disabled(clone!(@weak video_pad,
                                      @weak element,
                                      @weak video_appsrc => move |_, _| {
                element.imp().disable_video(&video_pad, &video_appsrc);
            }))
            .build();

        let subscriber = OpenTokSubscriber::new(subscriber_callbacks);

        subscriber
            .set_stream(stream)
            .map_err(|e| Error::SubscriberStreamSetupFailed(format!("{}", e)))?;

        if let Err(err) = session.subscribe(&subscriber) {
            gst::error!(
                CAT,
                obj: element,
                "Failed to subscribe to stream {:?}: {:?}",
                stream_id,
                err
            );
        }

        self.subscribers.insert(
            stream_id,
            Subscriber {
                subscriber,
                video_appsrc: video_appsrc.clone(),
                video_pad: video_pad.clone(),
            },
        );
        Ok((video_appsrc, video_pad))
    }

    fn stream_dropped(&mut self, element: &super::OpenTokSrc, stream: opentok::stream::Stream) {
        let stream_id = stream.id();
        gst::debug!(CAT, obj: element, "Stream dropped {}", stream_id);

        let subscriber = match self.subscribers.remove(&stream_id) {
            Some(subscriber) => subscriber,
            None => {
                gst::fixme!(
                    CAT,
                    obj: element,
                    "No registered subscriber info for stream id {:?}",
                    stream_id,
                );
                return;
            }
        };

        let bin = element
            .upcast_ref::<gst::Element>()
            .clone()
            .downcast::<gst::Bin>()
            .unwrap();

        subscriber.video_pad.set_active(false).unwrap();
        bin.set_locked_state(true);
        subscriber.video_appsrc.set_state(gst::State::Null).unwrap();
        let _ = subscriber.video_appsrc.state(None);
        bin.remove(&subscriber.video_appsrc).unwrap();
        let _ = bin.remove_pad(&subscriber.video_pad);

        if self.subscribers.is_empty() {
            gst::debug!(
                CAT,
                obj: element,
                "All subscribers gone. Releasing audio pad"
            );
            let audio_pad = element.static_pad("audio_stream").unwrap();
            audio_pad.set_active(false).unwrap();
            let ghost_pad = audio_pad.downcast_ref::<gst::GhostPad>().unwrap();
            let appsrc_pad = ghost_pad.target().unwrap();
            let appsrc = appsrc_pad.parent_element().unwrap();
            appsrc.set_state(gst::State::Null).unwrap();
            let _ = appsrc.state(None);
            bin.remove(&appsrc).unwrap();
            let _ = bin.remove_pad(&audio_pad);
        }
        bin.set_locked_state(false);
    }
}

fn generate_video_pad_name(subscribers: &HashMap<String, Subscriber>) -> std::string::String {
    let mut id = 0;
    for s in subscribers.values() {
        if let Some(stream) = s.subscriber.get_stream() {
            if stream.has_video() {
                id += 1;
            }
        }
    }
    format!("video_stream_{}", id)
}

impl OpenTokSrc {
    fn start(&self) -> Result<(), anyhow::Error> {
        gst::info!(CAT, imp: self, "OpenTokSrc initialization");

        async_std::task::block_on(
            self.state
                .lock()
                .unwrap()
                .credentials
                .load(Duration::from_secs(5)),
        )?;

        self.maybe_init_session().map_err(|error| anyhow!(error))
    }

    fn stop(&self) -> Result<(), gst::StateChangeError> {
        let obj = self.obj();
        if let Some(audio_appsrc) = obj.by_name("audio_appsrc") {
            obj.set_locked_state(true);
            audio_appsrc.set_state(gst::State::Null)?;
            let _ = audio_appsrc.state(None);
            obj.remove(&audio_appsrc).unwrap();
            let audio_pad = self.obj().static_pad("audio_stream").unwrap();
            obj.set_locked_state(false);
            self.obj().remove_pad(&audio_pad).map_err(|error| {
                gst::error!(CAT, imp: self, "Unable to remove audio pad: {:?}", error,);
                gst::StateChangeError
            })?;
        }

        for (_name, subscriber) in self.state.lock().unwrap().subscribers.drain() {
            obj.set_locked_state(true);
            subscriber.video_appsrc.set_state(gst::State::Null)?;
            let _ = subscriber.video_appsrc.state(None);
            obj.remove(&subscriber.video_appsrc).unwrap();
            obj.set_locked_state(false);
            self.obj()
                .remove_pad(&subscriber.video_pad)
                .map_err(|error| {
                    gst::error!(CAT, imp: self, "Unable to remove video pad: {:?}", error,);
                    gst::StateChangeError
                })?;
        }

        Ok(())
    }

    fn location(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .credentials
            .session_id()
            .map(|id| format!("opentok://{}", id))
    }

    fn proxy_pad_chain(
        &self,
        pad: &gst::ProxyPad,
        buffer: gst::Buffer,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        let res = gst::ProxyPad::chain_default(pad, Some(&*self.obj()), buffer);
        self.state
            .lock()
            .unwrap()
            .flow_combiner
            .update_pad_flow(pad, res)
    }

    fn init_audio(&self) -> Result<(), Error> {
        let obj = self.obj();
        let appsrc = gst::ElementFactory::make("appsrc")
            .name("audio_appsrc")
            .build()
            .map_err(|_| Error::MissingElement("appsrc"))?;
        appsrc.set_property("is-live", true);
        appsrc.set_property("format", gst::Format::Time);

        obj.add(&appsrc).map_err(|_| Error::AddElement("appsrc"))?;

        let appsrc_src_pad = appsrc
            .static_pad("src")
            .ok_or(Error::ElementPad("appsrc.src"))?;

        let templ = &self.audio_src_pad_template;
        let audio_pad = gst::GhostPad::builder_with_template(templ, Some(&templ.name()))
            .proxy_pad_chain_function({
                let this_weak = self.downgrade();
                move |pad, _parent, buffer| {
                    let this = match this_weak.upgrade() {
                        None => return Err(gst::FlowError::Flushing),
                        Some(this) => this,
                    };

                    this.proxy_pad_chain(pad, buffer)
                }
            })
            .build_with_target(&appsrc_src_pad)
            .map_err(|_| Error::PadConstruction("audio", "ghost pad".into()))?;

        self.state.lock().unwrap().flow_combiner.add_pad(&audio_pad);

        audio_pad
            .set_active(true)
            .map_err(|_| Error::PadActivation("audio ghost pad"))?;
        appsrc_src_pad.sticky_events_foreach(|event| {
            use std::ops::ControlFlow;
            audio_pad.push_event(event.clone());
            ControlFlow::Continue(gst::EventForeachAction::Keep)
        });

        if let Err(err) = obj.add_pad(&audio_pad) {
            gst::error!(CAT, imp: self, "Failed to add audio pad {:?}", err)
        }
        appsrc.sync_state_with_parent().unwrap();

        let appsrc_state_holder = Arc::new(Mutex::new(AppSrcStateHolder {
            allocator: None,
            query_allocator: true,
        }));
        let audio_device = AudioDevice::get_instance();
        audio_device
            .lock()
            .unwrap()
            .set_on_audio_sample_callback(Box::new(move |sample| {
                OpenTokSrc::push_audio_sample(&appsrc_state_holder, &appsrc, sample);
            }));

        Ok(())
    }

    fn init_session(&self, api_key: &str, session_id: &str, token: &str) -> Result<(), Error> {
        if self.state.lock().unwrap().session.is_some() {
            return Ok(());
        }

        self.init_audio()?;

        let video_src_pad_template = &self.video_src_pad_template;
        let state = self.state.clone();
        let session_callbacks = SessionCallbacks::builder()
            .on_stream_received(clone!(
                @weak self as this,
                @weak state,
                @weak video_src_pad_template,
                => move |session, stream| {
                    let has_video = stream.has_video();
                let (video_appsrc, video_pad) =
                    match state.lock().unwrap().stream_received(&this.obj(), &video_src_pad_template, session, stream) {
                        Ok((appsrc, pad)) => (appsrc, pad),
                        Err(err) => {
                            gst::error!(CAT, imp: this, "{}", err);
                            return;
                        }
                    };

                if has_video {
                    this.enable_video(&video_pad, &video_appsrc);
                }
            }))
            .on_stream_dropped(clone!(
                @weak self as this,
                @weak state,
            => move |_, stream| {
                state.lock().unwrap().stream_dropped(&this.obj(), stream);
            }))
            .on_error(clone!(
                @weak self as this
            => move |_, error, _| {
                gst::element_error!(&this.obj(), gst::ResourceError::Read, [error]);
            }))
            .build();
        match Session::new(api_key, session_id, session_callbacks) {
            Ok(session) => {
                let result = session
                    .connect(token)
                    .map_err(|e| Error::Init(format!("Connection error {:?}", e)));
                self.state.lock().unwrap().session = Some(session);
                result
            }
            Err(err) => Err(Error::Init(format!(
                "Failed to create OpenTok session `{:?}",
                err
            ))),
        }
    }

    fn maybe_init_session(&self) -> Result<(), Error> {
        let credentials = self.state.lock().unwrap().credentials.clone();
        if let Some(api_key) = credentials.api_key() {
            if let Some(session_id) = credentials.session_id() {
                if let Some(token) = credentials.token() {
                    return self.init_session(api_key, session_id, token);
                }
            }
        }
        Ok(())
    }

    fn enable_video(&self, video_pad: &gst::GhostPad, video_appsrc: &gst::Element) {
        gst::debug!(CAT, imp: self, "Enabling video pad");
        self.obj().add_pad(video_pad).unwrap();
        video_pad.set_active(true).unwrap();
        let appsrc_src_pad = video_appsrc.static_pad("src").unwrap();
        appsrc_src_pad.sticky_events_foreach(|event| {
            use std::ops::ControlFlow;
            video_pad.push_event(event.clone());
            ControlFlow::Continue(gst::EventForeachAction::Keep)
        });
        video_appsrc.sync_state_with_parent().unwrap();
    }

    fn disable_video(&self, video_pad: &gst::GhostPad, video_appsrc: &gst::Element) {
        gst::debug!(CAT, imp: self, "Disabling video pad");
        video_pad.set_active(false).unwrap();

        let obj = self.obj();
        obj.set_locked_state(true);
        video_appsrc.set_state(gst::State::Ready).unwrap();
        obj.set_locked_state(false);
        obj.remove_pad(video_pad).unwrap();
    }

    fn push_audio_sample(
        appsrc_state_holder: &Arc<Mutex<AppSrcStateHolder>>,
        appsrc: &gst::Element,
        sample: AudioSample,
    ) {
        let gst_format = "S16LE";
        let caps = gst::Caps::builder("audio/x-raw")
            .field("format", gst_format.to_string())
            .field("layout", "interleaved")
            .field("rate", sample.sampling_rate)
            .field("channels", sample.number_of_channels)
            .build();
        AppSrcStateHolder::push_sample(
            appsrc_state_holder,
            &caps,
            appsrc,
            sample.data.0.as_byte_slice(),
        );
    }

    fn push_video_frame(
        appsrc_state_holder: &Arc<Mutex<AppSrcStateHolder>>,
        appsrc: &gst::Element,
        frame: VideoFrame,
    ) {
        let data = frame.get_buffer().unwrap();
        let format = frame.get_format().unwrap();
        let width = frame.get_width().unwrap();
        let height = frame.get_height().unwrap();
        gst::trace!(
            CAT,
            obj: appsrc,
            "Pushing video frame with dimensions {}x{}",
            width,
            height
        );

        let caps = gst::Caps::builder("video/x-raw")
            .field("width", width)
            .field("height", height)
            .field("framerate", gst::Fraction::new(30, 1))
            .field("format", format!("{}", gst_from_otc_format(format)))
            .build();

        AppSrcStateHolder::push_sample(appsrc_state_holder, &caps, appsrc, data);
    }
}

#[glib::object_subclass]
impl ObjectSubclass for OpenTokSrc {
    const NAME: &'static str = "OpenTokSrc";
    type Type = super::OpenTokSrc;
    type ParentType = gst::Bin;
    type Interfaces = (gst::URIHandler,);

    fn with_class(klass: &Self::Class) -> Self {
        let video_src_pad_template = klass.pad_template("video_stream_%u").unwrap();
        let audio_src_pad_template = klass.pad_template("audio_stream").unwrap();
        let state = State {
            credentials: Default::default(),
            session: Default::default(),
            stream_id: Default::default(),
            subscribers: Default::default(),
            flow_combiner: gst_base::UniqueFlowCombiner::new(),
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            video_src_pad_template,
            audio_src_pad_template,
        }
    }
}

impl ObjectImpl for OpenTokSrc {
    fn constructed(&self) {
        self.parent_constructed();

        pipe_opentok_to_gst_log();

        self.obj()
            .set_suppressed_flags(gst::ElementFlags::SOURCE | gst::ElementFlags::SINK);
        self.obj().set_element_flags(gst::ElementFlags::SOURCE);
    }

    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            vec![
                glib::ParamSpecString::builder(
                    "api-key",
                )
                .blurb(
                    "OpenTok API key",
                )
                .build(),
                glib::ParamSpecString::builder(
                    "session-id"
                )
                .blurb(
                    "OpenTok session unique identifier",
                ).build(),
                glib::ParamSpecString::builder(
                    "token",
                )
                .blurb(
                    "OpenTok session token",
                )
                .build(),
                glib::ParamSpecString::builder(
                    "stream-id",
                ).blurb(
                    "Unique identifier of the OpenTok stream this source subscribes to",
                )
                .build(),
                glib::ParamSpecString::builder(
                    "location",
                )
                .blurb(
                    "OpenTok session location (i.e. opentok://<session id>/key=<api key>&token=<token>)",
                ).build(),
                glib::ParamSpecString::builder(
                    "demo-room-uri",
                ).blurb(
                    "URI of the opentok demo room, eg. https://opentokdemo.tokbox.com/room/rust345",
                ).build(),
                glib::ParamSpecBoolean::builder(
                    "is-live",
                )
                .default_value(true)
                .build(),
            ]
        });

        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        let log_if_err_fn = |res| {
            if let Err(err) = res {
                gst::error!(CAT, "Got error: {:?}", err);
            }
        };
        let mut state = self.state.lock().unwrap();
        match pspec.name() {
            "api-key" => {
                if let Ok(api_key) = value.get::<String>() {
                    log_if_err_fn(state.credentials.set_api_key(api_key));
                }
            }
            "session-id" => {
                if let Ok(session_id) = value.get::<String>() {
                    log_if_err_fn(state.credentials.set_session_id(session_id));
                }
            }
            "stream-id" => {
                if let Ok(stream_id) = value.get::<String>() {
                    state.set_stream_id(&self.obj(), stream_id);
                }
            }
            "token" => {
                if let Ok(token) = value.get::<String>() {
                    log_if_err_fn(state.credentials.set_token(token));
                }
            }
            "location" => {
                let location = value.get::<String>().expect("expected a string");
                if let Err(e) = state.set_location(&self.obj(), &location) {
                    gst::error!(CAT, imp: self, "Failed to set location: {:?}", e)
                }
            }
            "demo-room-uri" => {
                log_if_err_fn(
                    state
                        .credentials
                        .set_room_uri(value.get::<String>().expect("expected a string")),
                );
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "api-key" => {
                let state = self.state.lock().unwrap();
                state.credentials.api_key().to_value()
            }
            "session-id" => {
                let state = self.state.lock().unwrap();
                state.credentials.session_id().to_value()
            }
            "stream-id" => {
                let state = self.state.lock().unwrap();
                state
                    .stream_id
                    .as_ref()
                    .unwrap_or(&String::new())
                    .to_value()
            }
            "token" => {
                let state = self.state.lock().unwrap();
                state.credentials.token().to_value()
            }
            "location" => self.location().to_value(),
            "demo-room-uri" => self
                .state
                .lock()
                .unwrap()
                .credentials
                .room_uri()
                .map(|url| url.as_str())
                .to_value(),
            "is-live" => true.to_value(),
            _ => unimplemented!(),
        }
    }
}

impl GstObjectImpl for OpenTokSrc {}

impl ElementImpl for OpenTokSrc {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        init();
        static ELEMENT_METADATA: Lazy<gst::subclass::ElementMetadata> = Lazy::new(|| {
            gst::subclass::ElementMetadata::new(
                "OpenTok Source",
                "Source/Network",
                "Receives audio and video streams from an OpenTok session",
                "Fernando Jiménez Moreno <ferjm@igalia.com>, Philippe Normand <philn@igalia.com>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: Lazy<Vec<gst::PadTemplate>> = Lazy::new(|| {
            let (video_caps, audio_caps) = caps();

            let video_src_pad_template = gst::PadTemplate::new(
                "video_stream_%u",
                gst::PadDirection::Src,
                gst::PadPresence::Sometimes,
                &video_caps,
            )
            .unwrap();

            // OpenTok provides a single audio stream with the mix of all publisher
            // audios. There's no easy way to separate the audio per publisher.
            let audio_src_pad_template = gst::PadTemplate::new(
                "audio_stream",
                gst::PadDirection::Src,
                gst::PadPresence::Sometimes,
                &audio_caps,
            )
            .unwrap();

            vec![video_src_pad_template, audio_src_pad_template]
        });
        PAD_TEMPLATES.as_ref()
    }

    fn change_state(
        &self,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        gst::debug!(CAT, imp: self, "Changing state {:?}", transition);

        if transition == gst::StateChange::NullToReady {
            self.start().map_err(|error| {
                gst::error!(CAT, "Error changing state: {:?}", error);

                gst::StateChangeError
            })?;
        }

        let mut success = self.parent_change_state(transition)?;

        if transition == gst::StateChange::ReadyToPaused {
            success = gst::StateChangeSuccess::NoPreroll;
        }

        if transition == gst::StateChange::PausedToReady {
            self.stop()?;
        }

        Ok(success)
    }
}

impl BinImpl for OpenTokSrc {}

impl URIHandlerImpl for OpenTokSrc {
    const URI_TYPE: gst::URIType = gst::URIType::Src;

    fn protocols() -> &'static [&'static str] {
        &["opentok"]
    }

    fn uri(&self) -> Option<String> {
        self.location()
    }

    fn set_uri(&self, uri: &str) -> Result<(), glib::Error> {
        let mut state = self.state.lock().unwrap();
        state
            .set_location(&self.obj(), uri)
            .map_err(|e| glib::Error::new(gst::CoreError::Failed, &format!("{:?}", e)))
    }
}
