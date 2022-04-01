// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use gst::{gst_debug, gst_error, gst_warning};
use gst_video::VideoFormat;
use ipc_channel::ipc::IpcSender;
use opentok::log;
use opentok::video_frame::FrameFormat;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use url::Url;

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
    pub api_key: Option<String>,
    /// OpenTok Session identifier.
    /// You can think of a session as a “room” where clients can
    /// interact with one another in real-time — associated with
    /// this unique session ID.
    pub session_id: Option<String>,
    /// OpenTok Session token.
    /// A token is a unique authentication “key” that allows a
    /// client to join a session.
    pub token: Option<String>,
    /// Stream ID to subscribe to, if any.
    /// Only useful for subscribers.
    pub stream_id: Option<String>,
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
    let video_caps = gst::Caps::new_simple(
        "video/x-raw",
        &[
            (
                "format",
                &gst::List::new(&[
                    &VideoFormat::Nv12.to_str(),
                    &VideoFormat::Nv21.to_str(),
                    &VideoFormat::Uyvy.to_str(),
                    &VideoFormat::I420.to_str(),
                    &VideoFormat::Yuy2.to_str(),
                    &VideoFormat::Bgr.to_str(),
                ]),
            ),
            ("width", &gst::IntRange::<i32>::new(1, i32::MAX)),
            ("height", &gst::IntRange::<i32>::new(1, i32::MAX)),
            (
                "framerate",
                &gst::FractionRange::new(gst::Fraction::new(0, 1), gst::Fraction::new(i32::MAX, 1)),
            ),
        ],
    );

    let audio_caps = gst::Caps::new_simple(
        "audio/x-raw",
        &[
            ("format", &gst_audio::AUDIO_FORMAT_S16.to_str()),
            ("layout", &"interleaved"),
            ("rate", &44100),
            ("channels", &1),
        ],
    );

    (video_caps, audio_caps)
}

pub fn pipe_opentok_to_gst_log(category: gst::DebugCategory) {
    log::logger_callback(Box::new(move |msg| {
        if msg.contains("ERROR") {
            if msg.contains("Could not check wether there is a proxy") {
                return;
            }
            gst_error!(category, "{}", msg);
        } else if msg.contains("WARN") {
            gst_warning!(category, "{}", msg);
        } else {
            gst_debug!(category, "{}", msg);
        }
    }))
}
