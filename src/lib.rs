// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

pub mod common;
pub mod helper;
mod opentoksink;
#[path = "./opentoksink-remote/mod.rs"]
mod opentoksink_remote;
mod opentoksrc;
#[path = "./opentoksrc-remote/mod.rs"]
mod opentoksrc_remote;

use gst::glib;
pub use opentoksink::OpenTokSink;
pub use opentoksink_remote::OpenTokSinkRemote;
pub use opentoksrc::OpenTokSrc;
pub use opentoksrc_remote::OpenTokSrcRemote;

fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    opentoksink::register(plugin)?;
    opentoksrc::register(plugin)?;
    opentoksrc_remote::register(plugin)?;
    opentoksink_remote::register(plugin)
}

gst::plugin_define!(
    opentok,
    env!("CARGO_PKG_DESCRIPTION"),
    plugin_init,
    concat!(env!("CARGO_PKG_VERSION"), "-", env!("COMMIT_ID")),
    // FIXME: MPL-2.0 is only allowed since 1.18.3 (as unknown) and 1.20 (as known)
    "MPL",
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_REPOSITORY"),
    env!("BUILD_REL_DATE")
);
