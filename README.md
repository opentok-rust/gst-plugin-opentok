# gst-plugin-opentok

This is a GStreamer plugin to allow publishing and subscribing to OpenTok audio and video streams.

## Dependencies

gst-plugin-opentok relies on [opentok-rs](https://github.com/opentok-rust/opentok-rs). Check its [documentation](https://github.com/opentok-rust/opentok-rs#setting-up-your-environment) to setup your dev environment.

## Running the examples

All the examples can be run either by providing an OpenTok API key, session identifier and token that you can get from your [Vonage account dashboard](https://tokbox.com/account/#/) or by providing an URL to a room from [OpenTok's demo application](https://opentokdemo.tokbox.com).

You can see the examples cli options passing the `--help` argument.

```sh
cargo run --example publisher -- --help
gst-opentok-examples 0.1.0
gst-opentok examples

USAGE:
    publisher [OPTIONS]

OPTIONS:
        --duration <duration>
            Time in seconds the example will last

    -h, --help
            Print help information

    -k, --api-key <api_key>
            OpenTok/Vonage API key

        --opentok-url <opentok_url>
            Url of the form opentok://<session-id>/<stream-id>?key=<api-key>&token=<token>

        --remote
            Launch the GStreamer elements in a child process wrapper

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

    -V, --version
            Print version information

    -w, --audio-wave <audio_wave>
            Waveform of the oscillator we use as a audio test source for publishers
```

### Publisher

The `publisher` example will stream audio and video test signals to a remote OpenTok room. Once streaming is started an `opentok://` URI should be logged on the terminal output. You can use this URI with the `consumer` example for instance.

```sh
cargo run --example publisher -- --api-key $API_KEY --session-id $SESSION_ID --token $TOKEN
```

```sh
cargo run --example publisher -- --url https://opentokdemo.tokbox.com/room/rust
```

### Consumer

The `consumer` example will connect to a remote OpenTok room and fetch the audio/video streams of the room participants.

```sh
cargo run --example consumer -- --opentok-url "opentok://..."
```
