// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

#[derive(Debug, Default)]
pub struct Credentials {
    pub api_key: String,
    pub session_id: String,
    pub token: String,
}

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
        credentials.api_key = api_key.into();
    }
    if let Some(session_id) = matches.value_of("session_id") {
        credentials.session_id = session_id.into();
    }
    if let Some(token) = matches.value_of("token") {
        credentials.token = token.into();
    }

    if credentials.api_key.is_empty()
        || credentials.session_id.is_empty()
        || credentials.token.is_empty()
    {
        app.print_help().unwrap();
        return None;
    }

    let ipc_server = match matches.value_of("ipc_server") {
        Some(name) => name.into(),
        None => {
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
