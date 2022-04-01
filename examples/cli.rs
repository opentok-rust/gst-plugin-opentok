// Copyright (C) 2021 Fernando Jimenez Moreno <fjimenez@igalia.com>
// Copyright (C) 2021-2022 Philippe Normand <philn@igalia.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

#[derive(Clone, Debug, Default)]
pub struct Credentials {
    pub api_key: String,
    pub session_id: String,
    pub token: String,
}

#[derive(Clone)]
pub struct Settings {
    pub credentials: Credentials,
    pub stream_ids: Vec<String>,
    pub audio_wave: Option<String>,
    pub video_pattern: Option<String>,
    pub url: Option<String>,
    pub opentok_url: Option<String>,
    pub duration: Option<u64>,
    pub remote: bool,
}

pub async fn parse_cli() -> Option<Settings> {
    let yaml = yaml_rust::Yaml::Array(
        yaml_rust::YamlLoader::load_from_str(include_str!("cli.yaml")).ok()?,
    );
    let mut app = clap_serde::yaml_to_app(&yaml).ok()?;
    if let Ok(bin_name) = std::env::current_exe() {
        app.set_bin_name(bin_name.as_path().display().to_string());
    }
    let matches = app.clone().get_matches();

    let mut credentials = Credentials::default();
    let mut stream_ids: Vec<String> = vec![];

    let url = matches.value_of("url").map(|s| s.into());
    let opentok_url = matches.value_of("opentok_url").map(|s| s.into());

    if let Some(ref room_url) = url {
        let info_url = format!("{}/info", room_url);
        match surf::get(&info_url).recv_string().await {
            Ok(payload) => {
                let json = json::parse(&payload).expect("Invalid JSON");
                credentials.api_key = json["apiKey"].as_str().unwrap().into();
                credentials.session_id = json["sessionId"].as_str().unwrap().into();
                credentials.token = json["token"].as_str().unwrap().into();
            }
            Err(e) => eprintln!("Error while fetching {}: {:?}", &info_url, e),
        };
    } else {
        if let Some(api_key) = matches.value_of("api_key") {
            credentials.api_key = api_key.into();
        }
        if let Some(session_id) = matches.value_of("session_id") {
            credentials.session_id = session_id.into();
        }
        if let Some(token) = matches.value_of("token") {
            credentials.token = token.into();
        }
    }

    if opentok_url.is_none()
        && (credentials.api_key.is_empty()
            || credentials.session_id.is_empty()
            || credentials.token.is_empty())
    {
        eprintln!("No destination URL or credentials provided");
        return None;
    }

    if let Some(id) = matches.value_of("stream_id_1").map(|s| s.into()) {
        stream_ids.push(id);
    }

    if let Some(id) = matches.value_of("stream_id_2").map(|s| s.into()) {
        stream_ids.push(id);
    }

    let audio_wave = matches.value_of("audio_wave").map(|s| s.into());
    let video_pattern = matches.value_of("video_pattern").map(|s| s.into());
    let duration = matches
        .value_of("duration")
        .map(|s| s.parse::<u64>().unwrap());

    let remote = matches.is_present("remote");

    Some(Settings {
        credentials,
        stream_ids,
        audio_wave,
        video_pattern,
        url,
        opentok_url,
        duration,
        remote,
    })
}
