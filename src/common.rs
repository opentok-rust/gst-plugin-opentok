// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use anyhow::{anyhow, ensure};
use gst::glib;
use gst_video::VideoFormat;
use ipc_channel::ipc::IpcSender;
use once_cell::sync::Lazy;
use opentok::log;
use opentok::video_frame::FrameFormat;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Once};
use thiserror::Error;
use url::Url;

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "opentok-common",
        gst::DebugColorFlags::empty(),
        Some("OpenTok common"),
    )
});

#[derive(Debug, Error)]
pub enum Error {
    #[error("Cannot add element {0}")]
    AddElement(&'static str),
    #[error("Cannot get element pad {0}")]
    ElementPad(&'static str),
    #[error("OpenTok initialization error {0}")]
    Init(String),
    #[error("Invalid state: {0}")]
    InvalidState(&'static str),
    #[error("Cannot link elements: {0}")]
    LinkElements(&'static str),
    #[error("Missing element {0}. Check your GStreamer installation")]
    MissingElement(&'static str),
    #[error("Cannot find gst-opentok-helper binary in $PATH.")]
    MissingOpenTokRemoteBinary,
    #[error("Failed to launch OpenTokRemote container process")]
    OpenTokRemoteLaunchFailed,
    #[error("Cannot activate pad {0}")]
    PadActivation(&'static str),
    #[error("Cannot create pad {0} {1}")]
    PadConstruction(&'static str, String),
    #[error("Failed to set stream {0} on OpenTok subscriber")]
    SubscriberStreamSetupFailed(String),
    #[error("Not subscribing to {0}. We only care about {1}")]
    InvalidStream(String, String),
}

#[derive(Debug, Deserialize, Serialize)]
pub enum IpcMessage {
    Error(String),
    PublishedStream(String),
    Stream(StreamMessage),
    Terminate(),
}

#[derive(Debug, Deserialize, Serialize)]
pub enum StreamMessage {
    Audio(StreamMessageData),
    Video(StreamMessageData),
}

#[derive(Debug, Deserialize, Serialize)]
pub enum StreamMessageData {
    ShmSocketPathAdded(String, String, String),
    ShmSocketPathRemoved(String, IpcSender<()>),
    CapsChanged(String, String),
}

/// OpenTok session credentials.
#[derive(Clone, Debug, Default)]
pub struct Credentials {
    /// OpenTok API Key.
    api_key: Option<String>,
    /// OpenTok Session identifier.
    /// You can think of a session as a “room” where clients can
    /// interact with one another in real-time — associated with
    /// this unique session ID.
    session_id: Option<String>,
    /// OpenTok Session token.
    /// A token is a unique authentication “key” that allows a
    /// client to join a session.
    token: Option<String>,
    /// Stream ID to subscribe to, if any.
    /// Only useful for subscribers.
    stream_id: Option<String>,

    /// Uri of the room, conflicts with other fields
    room_uri: Option<Url>,
}

impl Credentials {

    pub fn is_complete(&self) -> bool {
        self.room_uri.is_some() || (
            self.api_key.is_some() &&
            self.session_id.is_some() &&
            self.token.is_some()
        )
    }

    pub fn room_uri(&self) -> Option<&Url> {
        self.room_uri.as_ref()
    }


    pub fn stream_id(&self) -> Option<&String> {
        self.stream_id.as_ref()
    }

    pub fn set_api_key(&mut self, key: String) -> Result<(), anyhow::Error> {
        ensure!(self.room_uri.is_none(), anyhow!("Can't set api_key when room_uri is set"));
        self.api_key = Some(key);
        Ok(())
    }

    pub fn set_session_id(&mut self, id: String) -> Result<(), anyhow::Error> {
        ensure!(self.room_uri.is_none(), anyhow!("Can't set session_id when room_uri is set"));
        self.session_id = Some(id);

        Ok(())
    }

    pub fn set_token(&mut self, token: String) -> Result<(), anyhow::Error> {
        ensure!(self.room_uri.is_none(), anyhow!("Can't set token when room_uri is set"));
        self.token = Some(token);
        Ok(())
    }

    pub fn set_stream_id(&mut self, stream_id: String) -> Result<(), anyhow::Error> {
        ensure!(self.room_uri.is_none(), anyhow!("Can't set stream_id when room_uri is set"));
        self.api_key = Some(stream_id);
        Ok(())
    }

    pub fn set_room_uri(&mut self, uri: String) -> Result<(), anyhow::Error> {
        ensure!(self.api_key.is_none() || self.session_id.is_none() || self.token.is_none(), anyhow!("Can't set `room_uri` when any other field is set"));

        self.room_uri = Some(Url::parse(&uri).map_err(|err| {
            glib::BoolError::new(
                format!("Malformed url {:?}", err),
                file!(),
                "set_location",
                line!(),
            )
        })?);

        Ok(())
    }

    pub async fn load(&mut self, timeout: std::time::Duration) -> Result<(), anyhow::Error> {
        gst::debug!(CAT, "Loading!!");
        if !self.room_uri.is_some() {
            return Ok(());
        }

        let info_url = format!("{}/info", self.room_uri.as_ref().unwrap());
        let json = async_std::future::timeout(timeout,
                async {
                    match surf::get(&info_url).recv_string().await {
                        Ok(payload) => {
                            json::parse(&payload).map_err(|err| anyhow!(err))
                        },
                        Err(e) => {
                            Err(anyhow!(e))
                        }
                    }
                }
        ).await.map_err(|err| anyhow!(err))??;

        self.api_key = Some(json["apiKey"].as_str().ok_or(anyhow!("No `apiKey` in json"))?.into());
        self.session_id = Some(json["sessionId"].as_str().ok_or(anyhow!("No `sessionId` key in json"))?.into());
        self.token = Some(json["token"].as_str().ok_or(anyhow!("No `token` key in json"))?.into());
        gst_debug!(CAT, "Loaded {:?}", self);

        Ok(())
    }

    pub fn api_key(&self) -> Option<&String> {
        self.api_key.as_ref()
    }

    pub fn session_id(&self) -> Option<&String> {
        self.session_id.as_ref()
    }

    pub fn token(&self) -> Option<&String> {
        self.token.as_ref()
    }
}

/// Extract credentials from urls of this form:
/// opentok://<session id>/<stream_id>?key=<key>&token=<token>
impl From<Url> for Credentials {
    fn from(url: Url) -> Credentials {
        let query_params: HashMap<_, _> = url.query_pairs().into_owned().collect();

        let api_key = query_params.get("key").cloned().unwrap_or_default();
        let token = query_params.get("token").cloned().unwrap_or_default();
        let session_id = url.host().map(|s| s.to_string()).unwrap_or_default();
        let mut stream_id = None;
        if let Some(path_segments) = url.path_segments().map(|c| c.collect::<Vec<_>>()) {
            if path_segments.len() == 1 {
                stream_id = Some(path_segments[0].into());
            }
        }
        Credentials {
            api_key: Some(api_key),
            session_id: Some(session_id),
            token: Some(token),
            stream_id,
            room_uri: None,
        }
    }
}


pub fn gst_from_otc_format(format: FrameFormat) -> VideoFormat {
    // FIXME: RGBA variants, mjpeg, raw (?)
    match format {
        FrameFormat::Nv12 => VideoFormat::Nv12,
        FrameFormat::Nv21 => VideoFormat::Nv21,
        FrameFormat::Uyvy => VideoFormat::Uyvy,
        FrameFormat::Yuv420P => VideoFormat::I420,
        FrameFormat::Yuy2 => VideoFormat::Yuy2,
        FrameFormat::Rgb24 => VideoFormat::Bgr,
        _ => unimplemented!(),
    }
}

pub fn otc_format_from_gst_format(format: VideoFormat) -> FrameFormat {
    match format {
        VideoFormat::Nv12 => FrameFormat::Nv12,
        VideoFormat::Nv21 => FrameFormat::Nv21,
        VideoFormat::Uyvy => FrameFormat::Uyvy,
        VideoFormat::I420 => FrameFormat::Yuv420P,
        VideoFormat::Yuy2 => FrameFormat::Yuy2,
        VideoFormat::Bgr => FrameFormat::Rgb24,
        _ => unimplemented!(),
    }
}

pub fn caps() -> (gst::Caps, gst::Caps) {
    let video_caps = gst::Caps::builder_full()
        .structure(
            gst::Structure::builder("video/x-raw")
                .field(
                    "format",
                    &gst::List::new(&[
                        &VideoFormat::Nv12.to_str(),
                        &VideoFormat::Nv21.to_str(),
                        &VideoFormat::Uyvy.to_str(),
                        &VideoFormat::I420.to_str(),
                        &VideoFormat::Yuy2.to_str(),
                        &VideoFormat::Bgr.to_str(),
                    ]),
                )
                .field("width", &gst::IntRange::<i32>::new(1, i32::MAX))
                .field("height", &gst::IntRange::<i32>::new(1, i32::MAX))
                .field(
                    "framerate",
                    &gst::FractionRange::new(
                        gst::Fraction::new(0, 1),
                        gst::Fraction::new(i32::MAX, 1),
                    ),
                )
                .build(),
        )
        .build();

    let audio_caps = gst::Caps::builder_full()
        .structure(
            gst::Structure::builder("audio/x-raw")
                .field("format", &gst_audio::AUDIO_FORMAT_S16.to_str())
                .field("layout", &"interleaved")
                .field("rate", &44100)
                .field("channels", &1)
                .build(),
        )
        .build();

    (video_caps, audio_caps)
}

pub fn pipe_opentok_to_gst_log(category: gst::DebugCategory) {
    log::logger_callback(Box::new(move |msg| {
        if msg.contains("ERROR") {
            if msg.contains("Could not check wether there is a proxy") {
                return;
            }
            gst::error!(category, "{}", msg);
        } else if msg.contains("WARN") {
            gst::warning!(category, "{}", msg);
        } else {
            gst::debug!(category, "{}", msg);
        }
    }))
}

static INIT: Once = Once::new();
pub fn init() {
    INIT.call_once(|| {
        gst::info!(CAT, "Initializing OpenTok");
        opentok::init().unwrap()
    });
}
