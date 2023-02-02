// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use anyhow::Result;
use gst::glib::{self, clone};
use gst::prelude::*;
use ipc_channel::ipc::{self, IpcSender};
use log::debug;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::{cli, IpcMessenger};
use gstopentok::common::{IpcMessage, StreamMessage, StreamMessageData};

pub struct Source {
    ipc_sender: Arc<Mutex<IpcSender<IpcMessage>>>,
}

impl Source {
    fn on_pad_created(pipeline: &gst::Pipeline, pad: &gst::Pad, socket_path: &str) -> Result<()> {
        let bin_name = format!("bin_{}", pad.name());
        let bin = gst::ElementFactory::make("bin").name(&bin_name).build()?;

        let queue = gst::ElementFactory::make("queue").build()?;
        let sink = gst::ElementFactory::make("shmsink")
            .name(&format!("sink_{}", pad.name()))
            .property("socket-path", &socket_path)
            .property("enable-last-sample", false)
            .build()?;

        let bin_ref = bin.downcast_ref::<gst::Bin>().unwrap();
        bin_ref.add_many(&[&queue, &sink])?;
        queue.link(&sink)?;

        let queue_sink_pad = queue.static_pad("sink").unwrap();
        let bin_sink_pad = gst::GhostPad::with_target(None, &queue_sink_pad)?;
        bin_sink_pad.set_active(true)?;
        bin.add_pad(&bin_sink_pad)?;

        pipeline.add(&bin)?;
        pad.link(&bin_sink_pad)?;
        bin.sync_state_with_parent()?;

        Ok(())
    }

    fn socket_path_for_pad(
        pipeline: &gst::Pipeline,
        pad: &gst::Pad,
    ) -> Result<std::string::String> {
        let bin_name = &format!("bin_{}", pad.name());
        let mut socket_path = "".to_string();
        if let Some(ref element) = pipeline.by_name(bin_name) {
            let bin = element.downcast_ref::<gst::Bin>().unwrap();
            let sink = bin
                .by_name(&format!("sink_{}", pad.name()))
                .expect("shmsink not found");
            socket_path = sink.property::<std::string::String>("socket-path");
        }
        Ok(socket_path)
    }

    fn cleanup_pad(pipeline: &gst::Pipeline, pad: &gst::Pad) {
        let bin_name = &format!("bin_{}", pad.name());
        if let Some(ref bin) = pipeline.by_name(bin_name) {
            bin.set_locked_state(true);
            bin.send_event(gst::event::Eos::new());
            bin.set_state(gst::State::Null).unwrap();
            let _ = bin.state(None);
            pipeline.remove(bin).unwrap();
            debug!("bin {} removed", bin_name);

            let bin_ref = pipeline.upcast_ref::<gst::Bin>();
            gst::debug_bin_to_dot_file_with_ts(
                bin_ref,
                gst::DebugGraphDetails::all(),
                "opentoksrc_wrapper_sink_removed",
            );
        }

        if let Some(ref opentok_element) = pipeline.by_name("opentok-element") {
            if opentok_element.num_src_pads() == 0 {
                debug!("All source pads gone, tearing down the pipeline");
                pipeline.send_event(gst::event::Eos::new());
            }
        }
    }

    pub fn new(
        pipeline: &gst::Pipeline,
        opentoksrc: &gst::Element,
        settings: &cli::Settings,
    ) -> Self {
        let server_name = settings.ipc_server.clone();

        // Create ipc channel to communicate with the main process, connect
        // to the main process one shot server and send the ipc receiver
        // where the messages from the child process will be sent.
        let (ipc_sender, ipc_receiver) = ipc::channel().unwrap();
        if let Ok(oneshot_sender) = IpcSender::connect(server_name) {
            oneshot_sender.send(ipc_receiver).unwrap();
        }

        let ipc_sender = Arc::new(Mutex::new(ipc_sender));

        opentoksrc.connect_pad_added(clone!(
            @weak pipeline,
            @weak ipc_sender,
        => move |_src, pad| {
            debug!("Pad added to opentoksrc {:?}", pad.name());

            let mut socket = std::env::temp_dir();
            socket.push(format!("opentok-{}-socket", Uuid::new_v4()));
            let socket_path = socket.to_str().unwrap().to_owned();

            let path_added = Source::on_pad_created(&pipeline, pad, &socket_path);
            let prev_caps = Arc::new(Mutex::new(None));
            pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |pad, info| {
                if let Some(gst::PadProbeData::Event(ref event)) = info.data {
                    if event.type_() == gst::EventType::Caps {
                        let caps = pad.current_caps().unwrap();
                        debug!("Notifying socket {} and caps {:?}", &socket_path, &caps);
                        ipc_sender
                            .lock()
                            .unwrap()
                            .send(match &path_added {
                                Ok(()) => {
                                    let pad_name = pad.name().to_string();
                                    let msg = if !prev_caps.lock().unwrap().is_none() {
                                        IpcMessage::Stream(
                                            if pad_name.contains("audio") {
                                                StreamMessage::Audio(StreamMessageData::CapsChanged(
                                                    caps.to_string(),
                                                    pad_name,
                                                ))
                                            } else {
                                                StreamMessage::Video(StreamMessageData::CapsChanged(
                                                    caps.to_string(),
                                                    pad_name,
                                                ))
                                            }
                                        )
                                    } else {
                                        IpcMessage::Stream(
                                            if pad_name.contains("audio") {
                                                StreamMessage::Audio(StreamMessageData::ShmSocketPathAdded(
                                                    socket_path.clone(),
                                                    caps.to_string(),
                                                    pad_name,
                                                ))
                                            } else {
                                                StreamMessage::Video(StreamMessageData::ShmSocketPathAdded(
                                                    socket_path.clone(),
                                                    caps.to_string(),
                                                    pad_name,
                                                ))
                                            }
                                        )
                                    };
                                    *prev_caps.lock().unwrap() = Some(caps.to_string());
                                    msg
                                }
                                Err(err) => IpcMessage::Error(err.to_string()),
                            })
                            .unwrap();
                    }
                }
                gst::PadProbeReturn::Ok
            });

            pipeline.set_state(gst::State::Playing).unwrap();
        }));

        opentoksrc.connect_pad_removed(clone!(
            @weak pipeline,
            @weak ipc_sender
        => move |_, pad| {
            debug!("Pad removed from opentoksrc: {:?}", pad.name());
            debug!("Notifying the other side");
            let (sender, receiver) = ipc::channel().unwrap();
            let _ = ipc_sender
                .lock()
                .unwrap()
                .send(match Source::socket_path_for_pad(&pipeline, pad) {
                    Ok(socket_path) => IpcMessage::Stream(
                        if pad.name().to_string().contains("audio") {
                            StreamMessage::Audio(StreamMessageData::ShmSocketPathRemoved(
                                socket_path,
                                sender,
                            ))
                        } else {
                            StreamMessage::Video(StreamMessageData::ShmSocketPathRemoved(
                                socket_path,
                                sender,
                            ))
                        }
                    ),
                    Err(err) => IpcMessage::Error(err.to_string()),
                });

            // Wait for the main process to tell us that the shmsrc has been removed and
            // hence we are good to remove the corresponding shmsink.
            if receiver.recv().is_ok() {
                Source::cleanup_pad(&pipeline, pad);
            }
        }));

        Self { ipc_sender }
    }
}

impl IpcMessenger for Source {
    fn send(&self, message: IpcMessage) {
        self.ipc_sender.lock().unwrap().send(message).unwrap();
    }
}
