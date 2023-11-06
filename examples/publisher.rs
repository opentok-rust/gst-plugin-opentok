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

use anyhow::Result;
use derive_more::{Display, Error};
use gst::glib;
use gst::prelude::*;

#[path = "./cli.rs"]
mod cli;

#[derive(Debug, Display, Error)]
#[display(fmt = "Missing element {}", _0)]
struct MissingElement(#[error(not(source))] &'static str);

#[derive(Debug, Display, Error)]
#[display(fmt = "Received error from {}: {} (debug: {:?})", src, error, debug)]
struct ErrorMessage {
    src: String,
    error: String,
    debug: Option<String>,
    source: glib::Error,
}

fn create_pipeline(settings: cli::Settings) -> Result<gst::Pipeline> {
    gst::init()?;

    let pipeline = gst::Pipeline::new(None);

    let videotestsrc = gst::ElementFactory::make("videotestsrc")
        .build()
        .map_err(|_| MissingElement("videotestsrc"))?;
    if let Some(ref pattern) = settings.video_pattern {
        videotestsrc.set_property_from_str("pattern", pattern);
    }

    let audiotestsrc = gst::ElementFactory::make("audiotestsrc")
        .build()
        .map_err(|_| MissingElement("audiotestsrc"))?;
    if let Some(ref wave) = settings.audio_wave {
        audiotestsrc.set_property_from_str("wave", wave);
    }

    let timeoverlay = gst::ElementFactory::make("timeoverlay")
        .build()
        .map_err(|_| MissingElement("timeoverlay"))?;

    let location = if let Some(opentok_url) = settings.opentok_url {
        opentok_url
    } else {
        let protocol = if settings.remote {
            "opentok-remote://"
        } else {
            "opentok://"
        };
        format!(
            "{}{}?key={}&token={}",
            protocol,
            settings.credentials.session_id,
            settings.credentials.api_key,
            settings.credentials.token
        )
    };

    let video_capsfilter = gst::ElementFactory::make("capsfilter").build().unwrap();
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "I420")
        .field("width", 1280i32)
        .field("height", 720i32)
        .build();

    video_capsfilter.set_property("caps", &caps);

    let opentoksink = gst::Element::make_from_uri(gst::URIType::Sink, &location, None)
        .map_err(|_| MissingElement("opentoksink"))?;

    opentoksink.connect("published-stream", false, |args| {
        if let Ok(stream_id) = args[1].get::<String>() {
            println!("published-stream signal received {:?}", stream_id);
        }
        if let Ok(url) = args[2].get::<String>() {
            println!("URL: {}", url);
        }
        None
    });

    pipeline
        .add_many(&[
            &videotestsrc,
            &timeoverlay,
            &video_capsfilter,
            &audiotestsrc,
            &opentoksink,
        ])
        .unwrap();

    let audio_sink = opentoksink.request_pad_simple("audio_sink").unwrap();
    let video_sink = opentoksink.request_pad_simple("video_sink").unwrap();

    gst::Element::link_many(&[&videotestsrc, &video_capsfilter, &timeoverlay])?;
    let videotestsrc_src_pad = timeoverlay.static_pad("src").unwrap();
    let audiotestsrc_src_pad = audiotestsrc.static_pad("src").unwrap();

    videotestsrc_src_pad.link(&video_sink).unwrap();
    audiotestsrc_src_pad.link(&audio_sink).unwrap();

    Ok(pipeline)
}

fn main_loop(pipeline: gst::Pipeline, settings: cli::Settings) -> Result<()> {
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
                            "publisher_state_changed_{:?}_{:?}",
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

    pipeline.set_state(gst::State::Playing)?;

    if let Some(duration) = settings.duration {
        let main_loop_ = main_loop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(duration));
            main_loop_.quit();
        });
    }

    main_loop.run();

    pipeline.set_state(gst::State::Null)?;
    bus.remove_watch()?;

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
