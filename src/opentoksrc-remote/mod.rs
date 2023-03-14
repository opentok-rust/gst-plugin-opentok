// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use gst::glib::{self, prelude::*};

mod imp;

glib::wrapper! {
    pub struct OpenTokSrcRemote(ObjectSubclass<imp::OpenTokSrcRemote>) @extends gst::Bin, gst::Element, gst::Object, @implements gst::URIHandler;
}

unsafe impl Send for OpenTokSrcRemote {}
unsafe impl Sync for OpenTokSrcRemote {}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "opentoksrc-remote",
        gst::Rank::None,
        OpenTokSrcRemote::static_type(),
    )
}
