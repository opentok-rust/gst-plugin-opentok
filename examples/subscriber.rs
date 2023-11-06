// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

extern crate anyhow;
extern crate gst;
extern crate gstopentok;

use anyhow::Result;
use derive_more::{Display, Error};
use gst::glib;
use gst::glib::clone;
use gst::prelude::*;

#[path = "./cli.rs"]
mod cli;

#[derive(Debug, Display, Error)]
#[display(fmt = "Received error from {}: {} (debug: {:?})", src, error, debug)]
struct ErrorMessage {
    src: String,
    error: String,
    debug: Option<String>,
    source: glib::Error,
}

fn create_pipeline(mut settings: cli::Settings) -> Result<gst::Pipeline> {
    gst::init()?;

    let pipeline = gst::Pipeline::new(None);

    let location = if let Some(opentok_url) = settings.opentok_url {
        opentok_url
    } else {
        let mut location = if settings.remote {
            format!("opentok-remote://{}", settings.credentials.session_id)
        } else {
            format!("opentok://{}", settings.credentials.session_id)
        };

        if !settings.stream_ids.is_empty() {
            location = format!("{}/{}", location, settings.stream_ids.pop().unwrap());
        }
        format!(
            "{}?key={}&token={}",
            location, settings.credentials.api_key, settings.credentials.token
        )
    };

    let opentoksrc = gst::Element::make_from_uri(gst::URIType::Src, &location, None).unwrap();

    pipeline.add_many(&[&opentoksrc]).unwrap();

    opentoksrc.connect_pad_added(clone!(
        @weak pipeline
    => move |_src, pad| {
        let bin = gst::ElementFactory::make("bin").name(&format!("bin_{}", pad.name())).build().unwrap();
        let bin_ref = bin.downcast_ref::<gst::Bin>().unwrap();
        let queue = gst::ElementFactory::make("queue").build().unwrap();
        bin_ref.add(&queue).unwrap();

        if pad.name() == "audio_stream" {
            let audioconvert = gst::ElementFactory::make("audioconvert").build().unwrap();
            let audioresample = gst::ElementFactory::make("audioresample").build().unwrap();
            let sink = gst::ElementFactory::make("autoaudiosink").build().unwrap();
            bin_ref
                .add_many(&[&audioconvert, &audioresample, &sink])
                .unwrap();
            gst::Element::link_many(&[&queue, &audioconvert, &audioresample, &sink]).unwrap();
        } else {
            let videoconvert = gst::ElementFactory::make("videoconvert").build().unwrap();
            let sink = gst::ElementFactory::make("autovideosink").build().unwrap();
            bin_ref.add_many(&[&videoconvert, &sink]).unwrap();
            gst::Element::link_many(&[&queue, &videoconvert, &sink]).unwrap();
        };

        pipeline.add(&bin).unwrap();

        let sink_pad = queue.static_pad("sink").unwrap();
        let bin_sink_pad = gst::GhostPad::with_target(None, &sink_pad).unwrap();
        bin_sink_pad.set_active(true).unwrap();
        bin.add_pad(&bin_sink_pad).unwrap();

        pad.link(&bin_sink_pad).unwrap();
        bin.sync_state_with_parent().unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();

        gst::debug_bin_to_dot_file_with_ts(
            pipeline.upcast_ref::<gst::Bin>(),
            gst::DebugGraphDetails::all(),
            format!("{}_added", pad.name()),
        );
    }));

    opentoksrc.connect_pad_removed(clone!(
        @weak pipeline
    => move |_, pad| {
        let bin_name = &format!("bin_{}", pad.name());
        if let Some(ref bin) = pipeline.by_name(bin_name) {
            bin.set_state(gst::State::Null).unwrap();
            let _ = bin.state(None);
            pipeline.remove(bin).unwrap();
            let bin_ref = pipeline.upcast_ref::<gst::Bin>();
            gst::debug_bin_to_dot_file_with_ts(
                bin_ref,
                gst::DebugGraphDetails::all(),
                format!("{}_removed", pad.name()),
            );
        }
    }));

    Ok(pipeline)
}

fn main_loop(pipeline: gst::Pipeline, settings: cli::Settings) -> Result<()> {
    let main_loop = glib::MainLoop::new(None, false);

    let bus = pipeline
        .bus()
        .expect("Pipeline without bus. Shouldn't happen!");

    let pipeline_ = pipeline.downgrade();
    let pipeline__ = pipeline.downgrade();
    let main_loop_clone = main_loop.clone();
    bus.add_watch(move |_, msg| {
        use gst::MessageView;

        let main_loop = &main_loop_clone;

        match msg.view() {
            MessageView::Eos(..) => main_loop.quit(),
            MessageView::Error(err) => {
                eprintln!(
                    "Error from {:?}: {} ({:?})",
                    err.src().map(|s| s.path_string()),
                    err.error(),
                    err.debug()
                );

                main_loop.quit();
            }
            MessageView::StateChanged(state) => {
                let pipeline = pipeline_.upgrade().unwrap();
                if state
                    .src()
                    .map(|s| s == pipeline.upcast_ref::<gst::Object>())
                    .unwrap_or(false)
                {
                    let bin_ref = pipeline.upcast_ref::<gst::Bin>();
                    gst::debug_bin_to_dot_file_with_ts(
                        bin_ref,
                        gst::DebugGraphDetails::all(),
                        format!(
                            "subscriber_state_changed_{:?}_{:?}",
                            state.old(),
                            state.current()
                        ),
                    );
                }
            }
            _ => (),
        }
        glib::Continue(true)
    })
    .expect("Failed to add bus watch");

    pipeline.set_state(gst::State::Ready)?;

    if let Some(duration) = settings.duration {
        let _ = glib::timeout_add_seconds_once(duration as u32, move || {
            let pipeline = pipeline__.upgrade().unwrap();
            pipeline.send_event(gst::event::Eos::new());
        });
    }

    main_loop.run();

    pipeline.set_state(gst::State::Null)?;

    Ok(())
}

#[async_std::main]
async fn main() {
    if let Some(settings) = cli::parse_cli().await {
        opentok::init().unwrap();
        if let Err(e) =
            create_pipeline(settings.clone()).and_then(|pipeline| main_loop(pipeline, settings))
        {
            eprintln!("Error! {}", e);
        }
        opentok::deinit().unwrap();
    }
}
