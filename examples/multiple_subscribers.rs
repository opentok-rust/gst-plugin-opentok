// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

// The OpenTok SDK creates a single AudioDevice instance that is shared across all
// instances of OpenTok that live in the same process. This means that we can only
// have a single subscriber per process.
//
// This example demonstrates how to use opentoksrc-remote to subscribe to two streams
// on two separate processes.
//
// Video is renderered on the screen and audio is dumped to disk.
//
// To run this example you need to provide an url and two stream IDs:
// cargo run -p gst-plugin-opentok --example multiple_subscribers -- \
//      --url https://opentokdemo.tokbox.com/room/rust \
//      --stream-id-1 A1ECEBE9-5996-42F1-9CEF-F7A65BD789AC \
//      --stream-id-2 F5C76DF7-4767-4901-96FB-729EA3422CA7

extern crate anyhow;
extern crate gst;
extern crate gstopentok;

use anyhow::Result;
use gst::prelude::*;
use gst::glib;

#[path = "./cli.rs"]
mod cli;

fn create_subscriber(pipeline: &gst::Pipeline, credentials: &cli::Credentials, stream_id: String) {
    let location = format!(
        "opentok-remote://{}/{}?key={}&token={}",
        credentials.session_id, stream_id, credentials.api_key, credentials.token
    );
    let opentoksrc = gst::Element::make_from_uri(gst::URIType::Src, &location, None).unwrap();

    pipeline.add(&opentoksrc).unwrap();

    let pipeline_ = pipeline.downgrade();
    opentoksrc.connect_pad_added(move |element, pad| {
        let pipeline = pipeline_.upgrade().unwrap();
        let bin = gst::ElementFactory::make("bin")
            .name(&format!("bin_{}_{}", element.name(), pad.name()))
            .build()
            .unwrap();
        let bin_ref = bin.downcast_ref::<gst::Bin>().unwrap();
        let queue = gst::ElementFactory::make("queue").build().unwrap();
        bin_ref.add(&queue).unwrap();

        if pad.name() == "audio_stream" {
            let audioconvert = gst::ElementFactory::make("audioconvert").build().unwrap();
            let audioresample = gst::ElementFactory::make("audioresample").build().unwrap();
            let enc = gst::ElementFactory::make("lamemp3enc").build().unwrap();
            let sink = gst::ElementFactory::make("filesink").build().unwrap();
            sink.set_property("location", format!("{}.mp3", stream_id));
            bin_ref
                .add_many(&[&audioconvert, &audioresample, &enc, &sink])
                .unwrap();
            gst::Element::link_many(&[&queue, &audioconvert, &audioresample, &enc, &sink]).unwrap();
        } else {
            let videoconvert = gst::ElementFactory::make("videoconvert").build().unwrap();
            let sink = gst::ElementFactory::make("autovideosink").build().unwrap();
            bin_ref.add_many(&[&videoconvert, &sink]).unwrap();
            gst::Element::link_many(&[&queue, &videoconvert, &sink]).unwrap();
        }

        pipeline.add(&bin).unwrap();

        let sink_pad = queue.static_pad("sink").unwrap();
        let bin_sink_pad = gst::GhostPad::with_target(None, &sink_pad).unwrap();
        bin_sink_pad.set_active(true).unwrap();
        bin.add_pad(&bin_sink_pad).unwrap();
        pad.link(&bin_sink_pad).unwrap();

        bin.sync_state_with_parent().unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
    });

    let pipeline_ = pipeline.downgrade();
    opentoksrc.connect_pad_removed(move |element, pad| {
        let bin_name = &format!("bin_{}_{}", element.name(), pad.name());
        if let Some(pipeline) = pipeline_.upgrade() {
            if let Some(ref bin) = pipeline.by_name(bin_name) {
                bin.set_locked_state(true);
                bin.send_event(gst::event::Eos::new());
                bin.set_state(gst::State::Null).unwrap();
                let _ = bin.state(None);
                pipeline.remove(bin).unwrap();
                let bin_ref = pipeline.upcast_ref::<gst::Bin>();
                gst::debug_bin_to_dot_file_with_ts(
                    bin_ref,
                    gst::DebugGraphDetails::all(),
                    &format!("{}_removed", pad.name()),
                );
            }
        }
    });
    opentoksrc.sync_state_with_parent().unwrap();
}

fn create_pipeline(settings: cli::Settings) -> Result<gst::Pipeline> {
    gst::init()?;

    let pipeline = gst::Pipeline::new(None);

    if settings.stream_ids.is_empty() {
        eprintln!("Missing stream ids");
        std::process::exit(-1);
    }

    if settings.url.is_none() {
        eprintln!("Missing url");
        std::process::exit(-1);
    }

    // Create subscriber.
    for stream_id in settings.stream_ids {
        create_subscriber(&pipeline, &settings.credentials, stream_id);
    }

    Ok(pipeline)
}

fn main_loop(pipeline: gst::Pipeline) -> Result<()> {
    let main_loop = glib::MainLoop::new(None, false);

    let bus = pipeline
        .bus()
        .expect("Pipeline without bus. Shouldn't happen!");

    let pipeline_ = pipeline.downgrade();
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
                if state.src().map(|s| s == pipeline.upcast_ref::<gst::Object>()).unwrap_or(false) {
                    let bin_ref = pipeline.upcast_ref::<gst::Bin>();
                    gst::debug_bin_to_dot_file_with_ts(
                        bin_ref,
                        gst::DebugGraphDetails::all(),
                        &format!(
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

    pipeline.set_state(gst::State::Paused)?;
    main_loop.run();

    bus.post(gst::message::Eos::new()).unwrap();
    pipeline.set_state(gst::State::Null)?;
    bus.remove_watch()?;

    Ok(())
}

#[async_std::main]
async fn main() {
    if let Some(settings) = cli::parse_cli().await {
        opentok::init().unwrap();
        if let Err(e) = create_pipeline(settings).and_then(main_loop) {
            eprintln!("Error! {}", e);
        }
        opentok::deinit().unwrap();
    }
}
