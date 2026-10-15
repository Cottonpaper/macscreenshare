//! A stand-in host for developing the client without a Mac.
//!
//! It speaks the real control protocol and streams synthetic (non-decodable)
//! frame payloads over UDP so the client's handshake, reassembly, stats and
//! reconnect paths can be exercised on any platform.

use std::time::Duration;

use anyhow::{bail, Result};
use macss_protocol::{
    packetize_frame, Codec, ControlMessage, PROTOCOL_VERSION, DEFAULT_CONTROL_PORT,
};
use macss_transport::{read_message, write_message, VideoSender};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut port = DEFAULT_CONTROL_PORT;
    let mut seconds = 10u64;
    let mut fps = 30u32;
    let mut token: Option<String> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => { port = arg(&args, i + 1, "--port")?.parse()?; i += 1; }
            "--seconds" => { seconds = arg(&args, i + 1, "--seconds")?.parse()?; i += 1; }
            "--fps" => { fps = arg(&args, i + 1, "--fps")?.parse()?; i += 1; }
            "--token" => { token = Some(arg(&args, i + 1, "--token")?.to_string()); i += 1; }
            other => tracing::warn!("ignoring argument {other}"),
        }
        i += 1;
    }
    let fps = fps.max(1);

    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("mock host listening on tcp/{port}; waiting for a client");

    let (stream, peer) = listener.accept().await?;
    let (mut rd, mut wr) = stream.into_split();
    tracing::info!("client connected from {peer}");

    // Handshake.
    match read_message(&mut rd).await? {
        Some(ControlMessage::Hello { protocol, client_name, token: client_token }) => {
            if protocol != PROTOCOL_VERSION {
                let _ = write_message(&mut wr, &ControlMessage::Error {
                    code: "protocol_mismatch".into(),
                    message: format!("host speaks {PROTOCOL_VERSION}"),
                }).await;
                bail!("client protocol {protocol} != {PROTOCOL_VERSION}");
            }
            if let Some(required) = &token {
                if client_token.as_deref() != Some(required.as_str()) {
                    let _ = write_message(&mut wr, &ControlMessage::Error {
                        code: "unauthorized".into(),
                        message: "bad token".into(),
                    }).await;
                    bail!("client sent a bad token");
                }
            }
            tracing::info!("hello from {client_name}");
        }
        other => bail!("expected hello, got {other:?}"),
    }

    write_message(&mut wr, &ControlMessage::HelloAck {
        protocol: PROTOCOL_VERSION,
        session_id: "mock-session".into(),
        host_name: "mock-mac".into(),
        display_width: 1280,
        display_height: 720,
        scale: 2.0,
        video_port: 0,
        codecs: vec![Codec::H264, Codec::Hevc],
        capabilities: vec![],
    })
    .await?;

    let mut sender: Option<VideoSender> = None;
    let mut frame_id: u32 = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(1000 / fps as u64));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            message = read_message(&mut rd) => {
                match message? {
                    None => {
                        tracing::info!("client disconnected");
                        break;
                    }
                    Some(ControlMessage::Ping { nonce }) => {
                        write_message(&mut wr, &ControlMessage::Pong { nonce }).await?;
                    }
                    Some(ControlMessage::Pong { .. }) => {}
                    Some(ControlMessage::StartStream { udp_port, codec, fps: want_fps, .. }) => {
                        let sender_conn = VideoSender::connect((peer.ip(), udp_port).into()).await?;
                        sender = Some(sender_conn);
                        tracing::info!(
                            "streaming {codec:?} to {}:{udp_port} at {want_fps} fps",
                            peer.ip()
                        );
                        write_message(&mut wr, &ControlMessage::StreamStarted {
                            codec: Codec::H264,
                            width: 1280,
                            height: 720,
                            fps: want_fps,
                        }).await?;
                    }
                    Some(ControlMessage::RequestKeyframe) => {
                        tracing::debug!("keyframe requested");
                    }
                    Some(ControlMessage::Clipboard { text }) => {
                        tracing::info!("clipboard from client: {} bytes", text.len());
                    }
                    Some(ControlMessage::Bye) => {
                        tracing::info!("client said bye");
                        break;
                    }
                    Some(other) => tracing::debug!("ignoring {other:?}"),
                }
            }

            _ = tick.tick() => {
                if let Some(sender) = sender.as_mut() {
                    let payload: Vec<u8> =
                        (0..8000u32).map(|i| ((i + frame_id) % 251) as u8).collect();
                    let keyframe = frame_id % (fps * 2) == 0;
                    let datagrams = packetize_frame(
                        frame_id,
                        Codec::H264,
                        keyframe,
                        frame_id * (1000 / fps),
                        &payload,
                    );
                    sender.send_datagrams(&datagrams).await?;
                    frame_id = frame_id.wrapping_add(1);
                    if frame_id % (fps * 5) == 0 {
                        let (frames, bytes) = sender.stats();
                        tracing::info!("sent {frames} frames ({bytes} bytes)");
                    }
                }
            }

            _ = &mut deadline => {
                tracing::info!("finished after {seconds}s; sending bye");
                let _ = write_message(&mut wr, &ControlMessage::Bye).await;
                break;
            }

            _ = tokio::signal::ctrl_c() => break,
        }
    }

    Ok(())
}

fn arg<'a>(args: &'a [String], index: usize, flag: &str) -> Result<&'a str> {
    args.get(index)
        .map(|s| s.as_str())
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}
