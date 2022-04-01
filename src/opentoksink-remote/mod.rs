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

glib::wrapper! {
    pub struct OpenTokSinkRemote(ObjectSubclass<imp::OpenTokSinkRemote>) @extends gst::Bin, gst::Element, gst::Object, @implements gst::URIHandler;
}

unsafe impl Send for OpenTokSinkRemote {}
unsafe impl Sync for OpenTokSinkRemote {}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "opentoksink-remote",
        gst::Rank::None,
        OpenTokSinkRemote::static_type(),
    )
}
