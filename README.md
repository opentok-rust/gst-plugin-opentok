# gst-plugin-opentok

This is a GStreamer plugin to allow publishing and subscribing to OpenTok audio and video streams.

## Dependencies

gst-plugin-opentok relies on [opentok-rs](https://github.com/opentok-rust/opentok-rs). Check its [documentation](https://github.com/opentok-rust/opentok-rs#setting-up-your-environment) to setup your dev environment.

## Running the examples

All the examples can be run either by providing an OpenTok API key, session identifier and token that you can get from your [Vonage account dashboard](https://tokbox.com/account/#/) or by providing an URL to a room from [OpenTok's demo application](https://opentokdemo.tokbox.com).

```sh
cargo run --example publisher -- --api-key $API_KEY --session-id $SESSION_ID --token $TOKEN
```

```sh
cargo run --example publisher -- --url https://opentokdemo.tokbox.com/room/rust
```

You can see the examples cli options passing the `--help` argument.

```sh
gst-opentok-examples 0.1.0

gst-opentok examples

USAGE:
    publisher [OPTIONS]

FLAGS:
    -h, --help       Print help information
    -V, --version    Print version information

OPTIONS:
    -k, --api-key <api_key>
            OpenTok/Vonage API key

    -s, --session-id <session_id>
            OpenTok/Vonage session ID

        --stream-id-1 <stream_id_1>
            First stream ID we want to subscribe to

        --stream-id-2 <stream_id_2>
            Second stream ID we want to subscribe to

    -t, --token <token>
            OpenTok/Vonage session token

    -u, --url <url>
            OpenTok/Vonage demo room url (i.e. https://opentokdemo.tokbox.com/room/rust)

    -v, --video-pattern <video_pattern>
            Type of video pattern to publish

    -w, --audio-wave <audio_wave>
            Waveform of the oscillator we use as a audio test source for publishers
```
