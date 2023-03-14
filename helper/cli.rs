// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use gstopentok::common::Credentials;

#[derive(Debug)]
pub enum Direction {
    Source,
    Sink,
}

#[derive(Debug)]
pub struct Settings {
    pub credentials: Credentials,
    pub stream_id: Option<String>,
    pub ipc_server: String,
    pub direction: Direction,
}

pub async fn parse_cli() -> Option<Settings> {
    let yaml = yaml_rust::Yaml::Array(
        yaml_rust::YamlLoader::load_from_str(include_str!("cli.yaml")).ok()?,
    );
    let mut app = clap_serde::yaml_to_app(&yaml).ok()?;
    let matches = app.clone().get_matches();

    let mut credentials = Credentials::default();
    if let Some(api_key) = matches.value_of("api_key") {
        credentials.set_api_key(api_key.into()).unwrap();
    }
    if let Some(session_id) = matches.value_of("session_id") {
        credentials.set_session_id(session_id.into()).unwrap();
    }
    if let Some(token) = matches.value_of("token") {
        credentials.set_token(token.into()).unwrap();
    }
    if let Some(room_uri) = matches.value_of("room_uri") {
        if let Err(err) = credentials.set_room_uri(room_uri.into()) {
            eprintln!("{:?}", err);
            app.print_help().unwrap();
            return None;
        }
    }

    if !credentials.is_complete() {
        eprintln!("===> Incomplete credentials!");
        app.print_help().unwrap();
        return None;
    }

    if let Err(err) = async_std::task::block_on(credentials.load(std::time::Duration::from_secs(5)))
    {
        eprintln!("could not load credentials: {:?}", err);
        app.print_help().unwrap();
        return None;
    }

    let ipc_server = match matches.value_of("ipc_server") {
        Some(name) => name.into(),
        None => {
            eprintln!("===> No ipc_server!");
            app.print_help().unwrap();
            return None;
        }
    };

    let direction = match matches.value_of("direction") {
        Some(value) => {
            if value == "src" {
                Direction::Source
            } else {
                Direction::Sink
            }
        }
        None => {
            eprintln!("===> No direction set");
            app.print_help().unwrap();
            return None;
        }
    };

    let stream_id = matches.value_of("stream_id").map(|s| s.into());

    Some(Settings {
        credentials,
        stream_id,
        ipc_server,
        direction,
    })
}
