// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use glib::prelude::*;

mod imp;

// The public Rust wrapper type for our element.
glib::wrapper! {
    pub struct OpenTokSink(ObjectSubclass<imp::OpenTokSink>) @extends gst::Bin, gst::Element, gst::Object, @implements gst::URIHandler;
}

// GStreamer elements need to be thread-safe. For the private implementation
// this is automatically enforced but for the public wrapper type we need
// to specify this manually.

unsafe impl Send for OpenTokSink {}
unsafe impl Sync for OpenTokSink {}

// Registers the type for our element, and then registers in GStreamer under
// the name "opentoksink" for being able to instantiate it via e.g.
// gst::ElementFactory::make().
pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "opentoksink",
        gst::Rank::None,
        OpenTokSink::static_type(),
    )
}
