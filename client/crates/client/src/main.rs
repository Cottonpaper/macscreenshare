//! macscreenshare client — connects to a macOS host and receives its screen.
//!
//! M0/M1 scope: handshake, stream startup, UDP video reception, stats. Video is
//! decoded only when built with the `ffmpeg` feature; otherwise packets are
//! counted. Input capture and rendering arrive in a later milestone.

use std::time::Duration;

use anyhow::Result;
use macss_client::{args, session};
use macss_transport::{send_magic_packet, Backoff};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = match args::parse_from(std::env::args().skip(1))? {
        Some(a) => a,
        None => {
            print!("{}", args::USAGE);
            return Ok(());
        }
    };

    if let Some(mac) = args.wol {
        match send_magic_packet(mac, args.wol_port).await {
            Ok(()) => tracing::info!("sent Wake-on-LAN magic packet to {mac:02x?}"),
            Err(e) => tracing::warn!("Wake-on-LAN failed: {e:#}"),
        }
        // Give the host a moment to come back before dialling.
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let mut backoff = Backoff::default();
    loop {
        match session::run_once(&args).await {
            Ok(()) => backoff.reset(),
            Err(e) => tracing::warn!("session ended: {e:#}"),
        }
        let delay = backoff.next_delay();
        tracing::info!("reconnecting in {delay:?} (Ctrl-C to quit)");
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("interrupted, exiting");
                return Ok(());
            }
        }
    }
}
