// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use gst::glib::{self, clone};
use gst::prelude::*;
use ipc_channel::ipc::{self, IpcSender};
use log::debug;
use std::path::Path;
use std::str::FromStr;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::{IpcMessenger, Stream};
use gstopentok::common::{Error, IpcMessage, StreamMessage, StreamMessageData};

use crate::cli;

pub struct Sink {
    ipc_sender: Arc<Mutex<IpcSender<IpcMessage>>>,
}

impl Sink {
    fn init_stream_pipeline(
        pipeline: &gst::Pipeline,
        stream: Stream,
        socket_path: std::string::String,
    ) -> Result<(), Error> {
        let path = Path::new(&socket_path);
        let socket_id = path.file_name().unwrap();

        let bin = gst::ElementFactory::make("bin")
            .name(socket_id.to_str().unwrap())
            .build()
            .unwrap();
        let bin_ref = bin.downcast_ref::<gst::Bin>().unwrap();

        let (name, caps) = match stream {
            Stream::Audio(ref caps) => ("audio_sink", gst::Caps::from_str(caps).unwrap()),
            Stream::Video(ref caps) => ("video_sink", gst::Caps::from_str(caps).unwrap()),
        };

        let opentoksink = pipeline.by_name("opentok-element").unwrap();
        debug!("Requesting pad {:?}", name);
        let sink_pad = opentoksink.request_pad_simple(name).unwrap();
        debug!("Got pad {:?}", name);

        let shmsrc = gst::ElementFactory::make("shmsrc")
            .name(&format!("shmsrc_{}", name))
            .build()
            .map_err(|_| Error::MissingElement("shmsrc"))?;
        shmsrc.set_property("is-live", true);
        shmsrc.set_property("do-timestamp", true);
        shmsrc.set_property("socket-path", &socket_path);

        let queue = gst::ElementFactory::make("queue")
            .build()
            .map_err(|_| Error::MissingElement("queue"))?;

        let capsfilter = gst::ElementFactory::make("capsfilter")
            .name(&format!("capsfilter_{}", name))
            .build()
            .map_err(|_| Error::MissingElement("capsfilter"))?;
        capsfilter.set_property("caps", &caps);

        bin_ref
            .add_many(&[&shmsrc, &capsfilter, &queue])
            .map_err(|_| Error::AddElement("shmsrc ! capsfilter ! queue"))?;
        shmsrc
            .link(&capsfilter)
            .map_err(|_| Error::LinkElements("shmsrc ! capsfilter"))?;

        capsfilter
            .link(&queue)
            .map_err(|_| Error::LinkElements("capsfilter ! queue"))?;

        let queue_src_pad = queue.static_pad("src").unwrap();
        let pad = gst::GhostPad::with_target(Some("src"), &queue_src_pad).unwrap();

        bin.add_pad(&pad).unwrap();
        pipeline.add(&bin).unwrap();

        pad.link(&sink_pad).unwrap();

        pipeline.set_state(gst::State::Playing).unwrap();

        bin.sync_state_with_parent().unwrap();
        opentoksink.sync_state_with_parent().unwrap();
        debug!("Setting pad {} active", name);
        pad.set_active(true).unwrap();

        let bin_ref = pipeline.upcast_ref::<gst::Bin>();
        gst::debug_bin_to_dot_file_with_ts(
            bin_ref,
            gst::DebugGraphDetails::all(),
            &format!("opentok_wrapper_init_{}", name,),
        );

        Ok(())
    }

    fn spawn_stream_thread(
        stream_type: String,
        receiver: Receiver<StreamMessageData>,
        pipeline: &gst::Pipeline,
        opentoksink: &gst::Element,
    ) {
        std::thread::spawn(clone!(
            @weak pipeline,
            @weak opentoksink
        => move || {
            loop {
                match receiver.try_recv() {
                    Ok(res) => match res {
                        StreamMessageData::ShmSocketPathAdded(socket_path, caps, _pad_name) => {
                            debug!("{} socket added: {}", stream_type, &socket_path);
                            let caps = match stream_type.as_str() {
                                "Audio" => Stream::Audio(caps),
                                "Video" => Stream::Video(caps),
                                _ => unreachable!(),
                            };
                            if let Err(err) = Sink::init_stream_pipeline(
                                &pipeline,
                                caps,
                                socket_path,
                            ) {
                                eprintln!("{}", &err.to_string());
                            }
                        },
                        StreamMessageData::ShmSocketPathRemoved(socket_path, _ipc_sender) => {
                            debug!("{} socket removed: {}", stream_type, &socket_path);
                            // TODO
                        },
                        _ => {}
                    },
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_micros(10000));
                    }
                }
            }
        }));
    }

    pub fn new(
        pipeline: &gst::Pipeline,
        opentoksink: &gst::Element,
        settings: &cli::Settings,
    ) -> Self {
        let (audio_thread_sender, audio_thread_receiver) = mpsc::channel();
        let (video_thread_sender, video_thread_receiver) = mpsc::channel();

        let audio_thread_sender = Arc::new(Mutex::new(audio_thread_sender));
        let video_thread_sender = Arc::new(Mutex::new(video_thread_sender));

        let ipc_server_name = settings.ipc_server.clone();

        let (child_to_parent_ipc_sender, child_to_parent_ipc_receiver) = ipc::channel().unwrap();

        let pipeline_weak = pipeline.downgrade();
        // Control thread
        thread::spawn(move || {
            debug!("Control thread running");

            let (parent_to_child_ipc_sender, parent_to_child_ipc_receiver) =
                ipc::channel().unwrap();
            if let Ok(oneshot_sender) = IpcSender::connect(ipc_server_name) {
                oneshot_sender
                    .send((parent_to_child_ipc_sender, child_to_parent_ipc_receiver))
                    .unwrap();
            }

            let pipeline = pipeline_weak.upgrade().unwrap();
            loop {
                match parent_to_child_ipc_receiver.try_recv() {
                    Ok(message) => {
                        debug!("IPC message received: {:?}", message);
                        match message {
                            IpcMessage::Stream(stream_message) => match stream_message {
                                StreamMessage::Audio(message) => {
                                    audio_thread_sender.lock().unwrap().send(message).unwrap();
                                }
                                StreamMessage::Video(message) => {
                                    video_thread_sender.lock().unwrap().send(message).unwrap();
                                }
                            },
                            IpcMessage::Terminate() => {
                                pipeline.send_event(gst::event::Eos::new());
                            }
                            _ => {}
                        }
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_micros(10000)),
                }
            }
        });

        Sink::spawn_stream_thread("Audio".into(), audio_thread_receiver, pipeline, opentoksink);

        Sink::spawn_stream_thread("Video".into(), video_thread_receiver, pipeline, opentoksink);

        let child_to_parent_ipc_sender = Arc::new(Mutex::new(child_to_parent_ipc_sender));
        let ipc_sender = child_to_parent_ipc_sender.clone();
        opentoksink.connect("published-stream", false, move |args| {
            if let Ok(stream_id) = args[1].get::<String>() {
                debug!(
                    "published-stream signal received on child process {:?}",
                    stream_id
                );
                child_to_parent_ipc_sender
                    .lock()
                    .unwrap()
                    .send(IpcMessage::PublishedStream(stream_id))
                    .unwrap()
            }
            None
        });

        Self { ipc_sender }
    }
}

impl IpcMessenger for Sink {
    fn send(&self, message: IpcMessage) {
        if let Err(e) = self.ipc_sender.lock().unwrap().send(message) {
            debug!("Failed to send IPC message: {:?}", e);
        }
    }
}
