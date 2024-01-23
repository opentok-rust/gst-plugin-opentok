use crate::common::IpcMessage;
use anyhow::Result;
use async_std::prelude::*;
use gst::{glib, prelude::*};
use once_cell::sync::Lazy;
use signal_hook::consts::signal::*;
use signal_hook_async_std::Signals;
use std::io::Error;
use std::sync::Arc;

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "opentoksink-remote-helper",
        gst::DebugColorFlags::empty(),
        Some("OpenTok Sink Remote helper"),
    )
});

#[path = "./cli.rs"]
mod cli;
#[path = "./sink.rs"]
mod sink;
#[path = "./source.rs"]
mod source;

trait IpcMessenger: Send + Sync {
    fn send(&self, message: IpcMessage);
}

/// Type of stream this source produces.
#[derive(Debug, PartialEq)]
enum Stream {
    Audio(String),
    Video(String),
}

async fn handle_signals(signals: Signals, main_loop: glib::MainLoop) {
    let mut signals = signals.fuse();
    while let Some(_signal) = signals.next().await {
        main_loop.quit();
    }
}

fn create_pipeline(settings: cli::Settings) -> Result<(gst::Pipeline, Arc<dyn IpcMessenger>)> {
    gst::init()?;

    let pipeline = gst::Pipeline::new(None);

    let credentials = &settings.credentials;
    let location = format!(
        "opentok://{}?key={}&token={}",
        &credentials.session_id().unwrap(),
        &credentials.api_key().unwrap(),
        &credentials.token().unwrap()
    );

    let uri_type = match settings.direction {
        cli::Direction::Source => gst::URIType::Src,
        cli::Direction::Sink => gst::URIType::Sink,
    };

    let element = gst::Element::make_from_uri(uri_type, &location, Some("opentok-element"))?;

    if let Some(ref stream_id) = settings.stream_id {
        element.set_property("stream-id", stream_id);
    }

    gst::debug!(CAT, "display_name: {:?}", settings.display_name);
    if let Some(ref display_name) = settings.display_name {
        element.set_property("display-name", display_name);
    }

    pipeline.add(&element)?;

    let messenger: Arc<dyn IpcMessenger> = match settings.direction {
        cli::Direction::Source => Arc::new(source::Source::new(&pipeline, &element, &settings)),
        cli::Direction::Sink => Arc::new(sink::Sink::new(&pipeline, &element, &settings)),
    };

    Ok((pipeline, messenger))
}

fn run_main_loop(
    main_loop: glib::MainLoop,
    pipeline: gst::Pipeline,
    ipc_messenger: Arc<dyn IpcMessenger>,
) -> Result<()> {
    let bus = pipeline
        .bus()
        .expect("Pipeline without bus. Shouldn't happen!");

    let pipeline_ = pipeline.downgrade();
    let main_loop_clone = main_loop.clone();
    bus.add_watch(move |_, msg| {
        use gst::MessageView;

        let main_loop = &main_loop_clone;

        match msg.view() {
            MessageView::Eos(..) => main_loop.quit(),
            MessageView::Error(err) => {
                ipc_messenger.send(IpcMessage::Error(err.error().to_string()));
                eprintln!(
                    "Error from {:?}: {} ({:?})",
                    err.src().map(|s| s.path_string()),
                    err.error(),
                    err.debug()
                );

                main_loop.quit();
            }
            MessageView::StateChanged(state) => {
                let pipeline = pipeline_.upgrade().unwrap();
                if state
                    .src()
                    .map(|s| s == pipeline.upcast_ref::<gst::Object>())
                    .unwrap_or(false)
                {
                    let bin_ref = pipeline.upcast_ref::<gst::Bin>();
                    gst::debug_bin_to_dot_file_with_ts(
                        bin_ref,
                        gst::DebugGraphDetails::all(),
                        format!(
                            "opentok_wrapper_state_changed_{:?}_{:?}",
                            state.old(),
                            state.current()
                        ),
                    );
                }
            }
            _ => (),
        }
        glib::Continue(true)
    })
    .expect("Failed to add bus watch");

    pipeline.set_state(gst::State::Ready)?;
    main_loop.run();

    bus.post(gst::message::Eos::new()).unwrap();

    pipeline.set_state(gst::State::Null)?;
    bus.remove_watch()?;

    Ok(())
}

pub async fn run() -> Result<(), Error> {
    env_logger::init();
    let main_loop = glib::MainLoop::new(None, false);

    let main_loop_clone = main_loop.clone();
    let signals = Signals::new([SIGHUP, SIGTERM, SIGINT, SIGQUIT])?;
    let handle = signals.handle();

    let signals_task = async_std::task::spawn(handle_signals(signals, main_loop_clone));

    match cli::parse_cli().await {
        None => eprintln!("Missing location"),
        Some(settings) => {
            opentok::init().unwrap();
            if let Err(e) = create_pipeline(settings)
                .and_then(|(pipeline, messenger)| run_main_loop(main_loop, pipeline, messenger))
            {
                eprintln!("Error! {}", e);
            }
            // FIXME: Figure out why this deadlocks
            // opentok::deinit().unwrap();
        }
    };

    handle.close();
    signals_task.await;
    Ok(())
}
