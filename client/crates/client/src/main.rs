//! macscreenshare client — connects to a macOS host and shows its screen.
//!
//! Two modes:
//! - windowed (default): renders into a winit window and forwards mouse/keyboard.
//! - headless (`--headless`): connects and logs, no GPU or input.

use std::sync::Arc;

use anyhow::Result;
use macss_client::args::{self, Args};
use macss_client::session::{self, SessionHooks};
use macss_decoder::FrameSink;
use macss_protocol::InputEvent;
use macss_viewer::{FrameSlot, UserEvent, ViewerApp};
use tracing_subscriber::EnvFilter;
use winit::event_loop::EventLoop;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let arguments = match args::parse_from(std::env::args().skip(1))? {
        Some(parsed) => parsed,
        None => {
            print!("{}", args::USAGE);
            return Ok(());
        }
    };

    if arguments.headless {
        run_headless(arguments)
    } else {
        run_windowed(arguments)
    }
}

fn run_headless(arguments: Args) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut hooks = SessionHooks::default();
        session::run_forever(&arguments, &mut hooks).await
    })
}

fn run_windowed(arguments: Args) -> Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let slot = Arc::new(FrameSlot::new(proxy));

    // The UI produces input; the network thread consumes it.
    let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel::<InputEvent>();

    let network_args = arguments.clone();
    let network_slot: Arc<dyn FrameSink> = slot.clone();

    std::thread::Builder::new()
        .name("macss-network".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!("could not start the network runtime: {error}");
                    return;
                }
            };
            runtime.block_on(async move {
                let mut hooks = SessionHooks {
                    frames: Some(network_slot),
                    input: Some(input_rx),
                };
                if let Err(error) = session::run_forever(&network_args, &mut hooks).await {
                    tracing::warn!("session stopped: {error:#}");
                }
            });
        })?;

    let mut app = ViewerApp::new(
        slot,
        Box::new(move |event| {
            let _ = input_tx.send(event);
        }),
    );

    event_loop.run_app(&mut app)?;

    if let Some(failure) = app.failure() {
        anyhow::bail!("viewer failed: {failure}");
    }
    Ok(())
}
