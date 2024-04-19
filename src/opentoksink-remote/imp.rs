// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use crate::common::{caps, Credentials, Error, IpcMessage, StreamMessage, StreamMessageData};

use gst::glib::subclass::prelude::*;
use gst::glib::{self, clone, ToValue};
use gst::prelude::*;
use gst::subclass::prelude::*;
use ipc_channel::ipc::{IpcOneShotServer, IpcReceiver, IpcSender};
use once_cell::sync::{Lazy, OnceCell};
use signal_child::Signalable;
use std::fmt::{self, Display};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use url::Url;
use uuid::Uuid;

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "opentoksink-remote",
        gst::DebugColorFlags::empty(),
        Some("OpenTok Sink Remote"),
    )
});

type IpcPeers = (IpcSender<IpcMessage>, IpcReceiver<IpcMessage>);

/// Stream type enumeration.
#[derive(Clone, Debug, PartialEq)]
enum StreamType {
    Audio,
    Video,
    Unknown__,
}

/// Element sink pad name to StreamType conversion.
impl From<&str> for StreamType {
    fn from(stream_type: &str) -> StreamType {
        match stream_type {
            "video_sink" => StreamType::Video,
            "audio_sink" => StreamType::Audio,
            _ => StreamType::Unknown__,
        }
    }
}

impl From<StreamType> for &'static str {
    fn from(stream_type: StreamType) -> &'static str {
        match stream_type {
            StreamType::Video => "video_sink",
            StreamType::Audio => "audio_sink",
            _ => "",
        }
    }
}

impl Display for StreamType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamType::Video => write!(f, "video_sink"),
            StreamType::Audio => write!(f, "audio_sink"),
            _ => Err(fmt::Error),
        }
    }
}

struct SignalEmitter {
    element: glib::object::WeakRef<gst::Element>,
}

impl SignalEmitter {
    pub fn emit_published_stream(&self, stream_id: &str, url: &str) {
        if let Some(element) = self.element.upgrade() {
            element.emit_by_name::<()>("published-stream", &[&stream_id, &url]);
        }
    }
}

#[derive(Default)]
pub struct OpenTokSinkRemote {
    /// Child process handler
    child_process: Arc<Mutex<Option<Child>>>,
    /// OpenTok session credentials (API key, session ID and token).
    credentials: Arc<Mutex<Credentials>>,
    /// OpenTok Stream identifier.
    /// We will be connecting to this stream only.
    stream_id: OnceCell<String>,
    /// Peer display name.
    display_name: Arc<Mutex<Option<String>>>,
    /// IPC sender to communicate with the child process.
    ipc_sender: Arc<Mutex<Option<IpcSender<IpcMessage>>>>,
    /// Published stream unique identifier.
    published_stream_id: Arc<Mutex<Option<String>>>,
    /// Helper to emit published stream signals from auxiliary
    /// threads.
    signal_emitter: Arc<Mutex<Option<SignalEmitter>>>,
    /// Boolean flag indicating whether the IPC thread should
    /// be running or not.
    ipc_thread_running: Arc<AtomicBool>,
    /// Audio stream bin.
    audio_bin: Mutex<Option<gst::Element>>,
    /// Video stream bin.
    video_bin: Mutex<Option<gst::Element>>,
}

impl OpenTokSinkRemote {
    fn location(&self) -> Option<String> {
        self.credentials
            .lock()
            .unwrap()
            .session_id()
            .map(|id| format!("opentok://{}", id))
    }

    fn set_location(&self, location: &str) -> Result<(), glib::BoolError> {
        gst::debug!(CAT, "Setting location to {}", location);
        let url = Url::parse(location).map_err(|err| {
            glib::BoolError::new(
                format!("Malformed url {:?}", err),
                file!(),
                "set_location",
                line!(),
            )
        })?;
        let credentials: Credentials = url.into();
        gst::debug!(CAT, "Credentials {:?}", credentials);
        if let Some(ref stream_id) = credentials.stream_id() {
            if !stream_id.is_empty() {
                self.set_stream_id(stream_id.to_string())?;
            }
        }

        *self.credentials.lock().unwrap() = credentials;
        Ok(())
    }

    fn set_stream_id(&self, id: String) -> Result<(), glib::BoolError> {
        gst::debug!(CAT, "Setting stream ID to {}", id);
        self.stream_id.set(id).map_err(|_| {
            glib::BoolError::new(
                "Stream ID can only be set once",
                file!(),
                "set_stream_id",
                line!(),
            )
        })
    }

    fn launch_child_process(
        &self,
        ipc_server_name: &str,
        api_key: &str,
        session_id: &str,
        token: &str,
    ) -> Result<(), Error> {
        let helper_exe = match std::env::current_exe() {
            Ok(mut exe) => {
                exe.pop();
                exe.push("gst-opentok-helper");
                if exe.exists() {
                    exe.into_os_string().into_string().unwrap()
                } else {
                    "gst-opentok-helper".to_string()
                }
            }
            Err(_) => "gst-opentok-helper".to_string(),
        };
        gst::info!(CAT, "Spawning child process {helper_exe}");
        let mut command = std::process::Command::new(helper_exe);
        command
            .arg("--api-key")
            .arg(api_key)
            .arg("--session-id")
            .arg(session_id)
            .arg("--token")
            .arg(token)
            .arg("--direction")
            .arg("sink")
            .arg("--ipc-server")
            .arg(ipc_server_name);
        if let Some(stream_id) = self.stream_id.get() {
            command.arg("--stream-id").arg(stream_id);
        }
        if let Some(display_name) = self.display_name.lock().unwrap().as_ref() {
            command.arg("--display-name").arg(display_name);
        }
        *self.child_process.lock().unwrap() = Some(
            command
                .spawn()
                .map_err(|_| Error::OpenTokRemoteLaunchFailed)?,
        );
        Ok(())
    }

    fn critical_error(
        error: &str,
        element: &gst::Element,
        child_process: &Arc<Mutex<Option<Child>>>,
    ) {
        gst::error!(CAT, obj: element, "{}", error);
        if let Some(mut child_process) = child_process.lock().unwrap().take() {
            let _ = child_process.interrupt();
        }
        if let Err(e) = element.post_message(gst::message::Error::new(
            gst::CoreError::Failed,
            &format!("Child process error {}", error),
        )) {
            gst::warning!(
                CAT,
                obj: element,
                "Unable to post message on the bus. {}",
                e
            );
        }
    }

    fn init(&self, api_key: &str, session_id: &str, token: &str) -> Result<(), Error> {
        gst::debug!(CAT, imp: self, "Init");
        // Spawn the child process and the auxiliary threads and hand over the
        // ipc server name.
        let (ipc_server, ipc_server_name): (IpcOneShotServer<IpcPeers>, String) =
            IpcOneShotServer::new().map_err(|_| Error::OpenTokRemoteLaunchFailed)?;

        self.launch_child_process(&ipc_server_name, api_key, session_id, token)?;

        let (_, (ipc_sender, ipc_receiver)) = ipc_server.accept().unwrap();
        gst::debug!(CAT, imp: self, "Got IPC sender");
        *self.ipc_sender.lock().unwrap() = Some(ipc_sender);

        let child_process = self.child_process.clone();
        let signal_emitter = self.signal_emitter.clone();
        let published_stream_id = self.published_stream_id.clone();
        let ipc_thread_running = &self.ipc_thread_running;
        let credentials = &self.credentials;

        thread::spawn(clone!(
            @weak self as this,
            @weak child_process,
            @weak ipc_thread_running,
            @weak credentials,
        => move || {
            gst::debug!(CAT, imp: this, "IPC thread running");
            ipc_thread_running.store(true, Ordering::Relaxed);
            let mut last_pong = std::time::Instant::now();
            let mut last_ping_sent = std::time::Instant::now();
            loop {
                if !ipc_thread_running.load(Ordering::Relaxed) {
                    break;
                }
                if last_ping_sent.elapsed().as_secs() > 1 {
                    gst::log!(CAT, imp: this, "Sending PING");
                    last_ping_sent = std::time::Instant::now();
                    this.ipc_sender.lock().unwrap().as_ref().unwrap().send(IpcMessage::Ping).unwrap();
                }
                match ipc_receiver.try_recv() {
                    Ok(message) => {
                        gst::debug!(CAT, imp: this, "IPC message received: {:?}", message);
                        match message {
                            IpcMessage::Error(err) => {
                                OpenTokSinkRemote::critical_error(
                                    &err,
                                    this.obj().upcast_ref(),
                                    &child_process
                                );
                                break;
                            },
                            IpcMessage::Pong => {
                                gst::log!(CAT, "Got pong");
                                last_pong = std::time::Instant::now();
                            },
                            IpcMessage::PublishedStream(stream_id) => {
                                if let Some(signal_emitter) = signal_emitter.lock().unwrap().as_ref() {
                                    *published_stream_id.lock().unwrap() = Some(stream_id.clone());
                                    let credentials = credentials.lock().unwrap().clone();
                                    let url = format!("opentok://{}/{}?key={}&token={}",
                                                      credentials.session_id().unwrap(),
                                                      stream_id,
                                                      credentials.api_key().unwrap(),
                                                      credentials.token().unwrap()
                                    );

                                    signal_emitter.emit_published_stream(&stream_id, &url);
                                }
                            },
                            _ => {},
                        }
                    },
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_micros(10000));
                        if last_pong.elapsed().as_secs() > 5 {
                            gst::error!(CAT, "No pong for 5sec, posting error");
                            OpenTokSinkRemote::critical_error(
                                "No pong for 5seconds",
                                this.obj().upcast_ref(),
                                &child_process
                            );
                            break;
                        }
                    }
                }
            }
            gst::debug!(CAT, imp: this, "IPC thread exiting");
        }));

        Ok(())
    }

    fn maybe_init(&self) -> Result<(), Error> {
        let credentials = self.credentials.lock().unwrap();
        if let Some(api_key) = credentials.api_key() {
            if let Some(session_id) = credentials.session_id() {
                if let Some(token) = credentials.token() {
                    return self.init(api_key, session_id, token);
                }
            }
        }
        Ok(())
    }

    fn teardown(&self) {
        self.ipc_thread_running.store(false, Ordering::Relaxed);
        if let Some(sender) = self.ipc_sender.lock().unwrap().take() {
            let msg = IpcMessage::Terminate();
            sender.send(msg).unwrap();
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for OpenTokSinkRemote {
    const NAME: &'static str = "OpenTokSinkRemote";
    type Type = super::OpenTokSinkRemote;
    type ParentType = gst::Bin;
    type Interfaces = (gst::URIHandler,);

    fn with_class(_klass: &Self::Class) -> Self {
        Self::default()
    }
}

impl ObjectImpl for OpenTokSinkRemote {
    fn constructed(&self) {
        self.parent_constructed();
        self.obj()
            .set_suppressed_flags(gst::ElementFlags::SOURCE | gst::ElementFlags::SINK);
        self.obj().set_element_flags(gst::ElementFlags::SINK);

        let element = self.obj().upcast_ref::<gst::Element>().downgrade();
        *self.signal_emitter.lock().unwrap() = Some(SignalEmitter { element });
    }

    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            vec![
                glib::ParamSpecString::builder( "location")
                    .blurb( "OpenTok session location (i.e. opentok-remote://<session id>/key=<api key>&token=<token>)")
                    .build(),
                glib::ParamSpecString::builder("stream-id")
                    .blurb( "Unique identifier of the OpenTok stream this sink is publishing")
                    .flags( glib::ParamFlags::READABLE)
                    .build(),
                glib::ParamSpecString::builder( "demo-room-uri")
                    .blurb( "URI of the opentok demo room, eg. https://opentokdemo.tokbox.com/room/rust345")
                    .flags( glib::ParamFlags::READWRITE)
                    .build(),
                glib::ParamSpecString::builder("display-name")
                    .blurb("Peer display name")
                    .flags(glib::ParamFlags::READWRITE)
                    .build(),
            ]
        });

        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        gst::trace!(CAT, imp: self, "Setting property {:?}", pspec.name());
        let log_if_err_fn = |res| {
            if let Err(err) = res {
                gst::error!(CAT, "Got error: {:?} while setting {}", err, pspec.name());
            }
        };

        match pspec.name() {
            "location" => {
                let location = value.get::<String>().expect("expected a string");
                if let Err(e) = self.set_location(&location) {
                    gst::error!(CAT, imp: self, "Failed to set location: {:?}", e)
                }
            }
            "demo-room-uri" => {
                log_if_err_fn(
                    self.credentials
                        .lock()
                        .unwrap()
                        .set_room_uri(value.get::<String>().expect("expected a string")),
                );
            }
            "display-name" => {
                *self.display_name.lock().unwrap() = value.get::<Option<String>>().unwrap()
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "location" => self.location().to_value(),
            "stream-id" => self
                .published_stream_id
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| "".into())
                .to_value(),
            "demo-room-uri" => self
                .credentials
                .lock()
                .unwrap()
                .room_uri()
                .map(|url| url.as_str())
                .to_value(),
            "display-name" => self.display_name.lock().unwrap().to_value(),
            _ => unimplemented!(),
        }
    }

    fn signals() -> &'static [glib::subclass::Signal] {
        static SIGNALS: Lazy<Vec<glib::subclass::Signal>> = Lazy::new(|| {
            vec![glib::subclass::Signal::builder("published-stream")
                .param_types([String::static_type(), String::static_type()])
                .build()]
        });

        SIGNALS.as_ref()
    }
}

impl GstObjectImpl for OpenTokSinkRemote {}

impl ElementImpl for OpenTokSinkRemote {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gst::subclass::ElementMetadata> = Lazy::new(|| {
            gst::subclass::ElementMetadata::new(
                "OpenTok Sink Remote",
                "Sink/Network",
                "Send audio and video streams to an OpenTok session in a separate process",
                "Philippe Normand <philn@igalia.com>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: Lazy<Vec<gst::PadTemplate>> = Lazy::new(|| {
            let (video_caps, audio_caps) = caps();

            let video_sink_pad_template = gst::PadTemplate::new(
                "video_sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Request,
                &video_caps,
            )
            .unwrap();

            let audio_sink_pad_template = gst::PadTemplate::new(
                "audio_sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Request,
                &audio_caps,
            )
            .unwrap();

            vec![video_sink_pad_template, audio_sink_pad_template]
        });
        PAD_TEMPLATES.as_ref()
    }

    fn request_new_pad(
        &self,
        template: &gst::PadTemplate,
        _name: Option<&str>,
        _caps: Option<&gst::Caps>,
    ) -> Option<gst::Pad> {
        let stream_type: StreamType = template.name_template().into();

        gst::debug!(CAT, imp: self, "Requesting new pad {:?}", stream_type);

        let setup_sink = || -> Result<gst::Pad, Error> {
            let mut socket = std::env::temp_dir();
            socket.push(format!("opentok-{}-socket", Uuid::new_v4()));
            let socket_path = socket.to_str().unwrap().to_owned();

            let bin = gst::ElementFactory::make("bin")
                .name(&format!("bin_{}", &stream_type))
                .build()
                .unwrap();

            match stream_type {
                StreamType::Audio => *self.audio_bin.lock().unwrap() = Some(bin.clone()),
                StreamType::Video => *self.video_bin.lock().unwrap() = Some(bin.clone()),
                StreamType::Unknown__ => unreachable!(),
            }

            let queue = gst::ElementFactory::make("queue")
                .build()
                .map_err(|_| Error::MissingElement("queue"))?;
            let sink = gst::ElementFactory::make("shmsink")
                .name(&format!("sink_{}", &stream_type))
                .build()
                .map_err(|_| Error::MissingElement("shmsink"))?;
            sink.set_property("socket-path", &socket_path);
            sink.set_property("enable-last-sample", false);

            let bin_ref = bin.downcast_ref::<gst::Bin>().unwrap();
            bin_ref.add_many(&[&queue, &sink]).unwrap();
            queue.link(&sink).unwrap();

            let queue_sink_pad = queue.static_pad("sink").unwrap();
            let bin_sink_pad =
                gst::GhostPad::with_target(None, &queue_sink_pad).expect("bin sink with target");
            bin_sink_pad
                .set_active(true)
                .expect("activate bin sink pad");
            bin.add_pad(&bin_sink_pad)
                .map_err(|_| Error::AddElement("bin sink pad"))?;

            let element_bin_ref = self.obj().clone().upcast::<gst::Bin>();
            element_bin_ref
                .add(&bin)
                .map_err(|_| Error::AddElement("bin sink pad"))?;
            bin.sync_state_with_parent().unwrap();

            let pad = gst::GhostPad::from_template(template, Some(&format!("{}", &stream_type)));
            pad.set_target(Some(&bin_sink_pad))
                .map_err(|_| Error::PadConstruction("shm_bin_sink", format!("{:?}", template)))?;

            pad.set_active(true).expect("activate bin sink pad");
            self.obj()
                .add_pad(&pad)
                .map_err(|_| Error::AddElement("bin sink pad"))?;

            let sender = self.ipc_sender.clone();
            let prev_caps = Arc::new(Mutex::new(None));
            pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |pad, info| {
                if let Some(gst::PadProbeData::Event(ref event)) = info.data {
                    if let gst::EventView::Caps(caps) = event.view() {
                        let caps = caps.caps_owned();
                        gst::debug!(
                            CAT,
                            "Notifying socket {} and caps {:?}",
                            &socket_path,
                            &caps
                        );
                        let msg = IpcMessage::Stream(if !prev_caps.lock().unwrap().is_none() {
                            match stream_type {
                                StreamType::Audio => {
                                    StreamMessage::Audio(StreamMessageData::CapsChanged(
                                        caps.to_string(),
                                        pad.name().to_string(),
                                    ))
                                }
                                StreamType::Video => {
                                    StreamMessage::Video(StreamMessageData::CapsChanged(
                                        caps.to_string(),
                                        pad.name().to_string(),
                                    ))
                                }
                                _ => unreachable!(),
                            }
                        } else {
                            match stream_type {
                                StreamType::Audio => {
                                    StreamMessage::Audio(StreamMessageData::ShmSocketPathAdded(
                                        socket_path.clone(),
                                        caps.to_string(),
                                        pad.name().to_string(),
                                    ))
                                }
                                StreamType::Video => {
                                    StreamMessage::Video(StreamMessageData::ShmSocketPathAdded(
                                        socket_path.clone(),
                                        caps.to_string(),
                                        pad.name().to_string(),
                                    ))
                                }
                                _ => unreachable!(),
                            }
                        });
                        *prev_caps.lock().unwrap() = Some(caps.to_string());
                        if let Some(ref sender) = *sender.lock().unwrap() {
                            sender.send(msg).unwrap();
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            });

            Ok(pad.upcast())
        };

        match setup_sink() {
            Ok(pad) => Some(pad),
            Err(err) => {
                gst::error!(CAT, imp: self, "{}", err);
                None
            }
        }
    }

    fn release_pad(&self, pad: &gst::Pad) {
        gst::debug!(CAT, imp: self, "Release pad {:?}", pad.name());

        let bin = match pad.name().as_str().into() {
            StreamType::Audio => self.audio_bin.lock().unwrap().take(),
            StreamType::Video => self.video_bin.lock().unwrap().take(),
            StreamType::Unknown__ => unreachable!(),
        };

        if let Some(ref bin) = bin {
            if self.obj().by_name(&bin.name()).is_some() {
                bin.set_state(gst::State::Null).unwrap();
                let _ = bin.state(None);
                let _ = self.remove_element(bin);
            }
        }

        let _ = self.obj().remove_pad(pad);
    }

    fn change_state(
        &self,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        gst::debug!(CAT, imp: self, "Changing state {:?}", transition);

        if transition == gst::StateChange::ReadyToPaused {
            async_std::task::block_on(
                self.credentials
                    .lock()
                    .unwrap()
                    .load(std::time::Duration::from_secs(5)),
            )
            .map_err(|error| {
                gst::error!(CAT, "Error changing state: {:?}", error);

                gst::StateChangeError
            })?;

            if let Err(e) = self.maybe_init() {
                gst::error!(
                    CAT,
                    imp: self,
                    "Failed to initialize OpenTok session: {:?}",
                    e
                )
            }
        }

        if transition == gst::StateChange::ReadyToNull {
            self.teardown();
        }

        let success = self.parent_change_state(transition)?;
        gst::debug!(CAT, imp: self, "State changed {:?}", transition);
        Ok(success)
    }
}

impl BinImpl for OpenTokSinkRemote {
    fn handle_message(&self, message: gst::Message) {
        if matches!(message.view(), gst::MessageView::Error(..))
            && !self.ipc_thread_running.load(Ordering::Relaxed)
        {
            gst::warning!(
                CAT,
                "Dropping error message because IPC thread is not running"
            );
            return;
        }

        self.parent_handle_message(message)
    }
}

impl URIHandlerImpl for OpenTokSinkRemote {
    const URI_TYPE: gst::URIType = gst::URIType::Sink;

    fn protocols() -> &'static [&'static str] {
        &["opentok-remote"]
    }

    fn uri(&self) -> Option<String> {
        self.location()
    }

    fn set_uri(&self, uri: &str) -> Result<(), glib::Error> {
        self.set_location(uri)
            .map_err(|e| glib::Error::new(gst::CoreError::Failed, &format!("{:?}", e)))?;
        self.maybe_init()
            .map_err(|e| glib::Error::new(gst::CoreError::Failed, &format!("{:?}", e)))
    }
}

impl Drop for OpenTokSinkRemote {
    fn drop(&mut self) {
        gst::debug!(CAT, "Dropping OpenTokSinkRemote");

        self.teardown();

        if let Some(mut child_process) = self.child_process.lock().unwrap().take() {
            let _ = child_process.interrupt();
            gst::debug!(CAT, "Interrupted child process");
        }
    }
}
