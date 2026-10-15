//! One client session: handshake, stream setup, receive loop, heartbeat.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use macss_decoder::{backend_name, default_decoder, VideoDecoder};
use macss_protocol::{ControlMessage, PROTOCOL_VERSION};
use macss_transport::{parse_host_port, read_message, write_message, VideoReceiver};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::args::Args;

const HEARTBEAT: Duration = Duration::from_secs(2);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_POLL: Duration = Duration::from_millis(250);

/// Connect once and run until the connection ends. Returns `Ok(())` when the
/// host closes cleanly so the caller can reconnect.
pub async fn run_once(args: &Args) -> Result<()> {
    let addr = parse_host_port(&args.host, args.control_port)?;
    tracing::info!("connecting to {addr}");
    let stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect to {addr}"))?;
    stream.set_nodelay(true).ok();
    let (mut rd, mut wr) = stream.into_split();

    let (tx, mut rx) = mpsc::channel::<ControlMessage>(256);
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if let Err(e) = write_message(&mut wr, &m).await {
                tracing::debug!("control write failed: {e:#}");
                break;
            }
        }
    });

    let result = handshake_and_stream(args, &mut rd, &tx).await;

    // Best-effort goodbye, then let the writer drain.
    let _ = tx.send(ControlMessage::Bye).await;
    drop(tx);
    let _ = writer.await;

    result
}

async fn handshake_and_stream(
    args: &Args,
    rd: &mut tokio::net::tcp::OwnedReadHalf,
    tx: &mpsc::Sender<ControlMessage>,
) -> Result<()> {
    tx.send(ControlMessage::Hello {
        protocol: PROTOCOL_VERSION,
        client_name: args.client_name.clone(),
        token: args.token.clone(),
    })
    .await?;

    let codecs = match read_message(rd).await? {
        Some(ControlMessage::HelloAck {
            protocol,
            session_id,
            host_name,
            display_width,
            display_height,
            scale,
            video_port: _,
            codecs,
        }) => {
            if protocol != PROTOCOL_VERSION {
                bail!("host speaks protocol {protocol}, we speak {PROTOCOL_VERSION}");
            }
            tracing::info!(
                "connected to {host_name} (session {session_id}) display {display_width}x{display_height} @{scale}x"
            );
            codecs
        }
        Some(ControlMessage::Error { code, message }) => {
            bail!("host rejected connection: {code}: {message}")
        }
        Some(other) => bail!("unexpected message during handshake: {other:?}"),
        None => bail!("host closed the connection during handshake"),
    };

    let codec = if codecs.contains(&args.codec) {
        args.codec
    } else {
        let fallback = *codecs
            .first()
            .ok_or_else(|| anyhow::anyhow!("host offered no codecs"))?;
        tracing::warn!("host does not support {:?}; using {fallback:?}", args.codec);
        fallback
    };

    let mut video = VideoReceiver::bind(args.video_port)
        .await
        .context("bind local video socket")?;
    let udp_port = video.local_port()?;

    tx.send(ControlMessage::StartStream {
        codec,
        fps: args.fps,
        max_bitrate_kbps: args.bitrate_kbps,
        udp_port,
    })
    .await?;
    tracing::info!("requested {codec:?} @ {} fps, <= {} kbit/s, udp/{udp_port}", args.fps, args.bitrate_kbps);

    let mut decoder = default_decoder().context("create decoder")?;
    tracing::info!("decoder: {}", backend_name());

    receive_loop(args, rd, tx, &mut video, decoder.as_mut()).await
}

async fn receive_loop(
    args: &Args,
    rd: &mut tokio::net::tcp::OwnedReadHalf,
    tx: &mpsc::Sender<ControlMessage>,
    video: &mut VideoReceiver,
    decoder: &mut dyn VideoDecoder,
) -> Result<()> {
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut nonce: u64 = 0;
    let mut last_rx = Instant::now();

    let mut frames: u64 = 0;
    let mut bytes: u64 = 0;
    let mut decoded: u64 = 0;
    let mut last_frame_id: Option<u32> = None;
    let mut keyframe_requests: u64 = 0;
    let mut window_start = Instant::now();

    loop {
        tokio::select! {
            msg = read_message(rd) => {
                last_rx = Instant::now();
                match msg? {
                    None => {
                        tracing::info!("host closed the connection");
                        return Ok(());
                    }
                    Some(ControlMessage::Ping { nonce }) => {
                        tx.send(ControlMessage::Pong { nonce }).await?;
                    }
                    Some(ControlMessage::Pong { .. }) => {}
                    Some(ControlMessage::Stats { fps, bitrate_kbps, encode_ms, dropped }) => {
                        tracing::info!(
                            "host: {fps:.1} fps, {bitrate_kbps} kbit/s, encode {encode_ms:.1} ms, dropped {dropped}"
                        );
                    }
                    Some(ControlMessage::Clipboard { text }) => {
                        tracing::info!("clipboard from host: {} bytes", text.len());
                    }
                    Some(ControlMessage::Error { code, message }) => {
                        bail!("host error: {code}: {message}");
                    }
                    Some(ControlMessage::Bye) => {
                        tracing::info!("host said goodbye");
                        return Ok(());
                    }
                    Some(other) => tracing::debug!("ignoring control message: {other:?}"),
                }
            }

            frame = video.recv_frame(READ_POLL) => {
                last_rx = Instant::now();
                if let Some(frame) = frame? {
                    // Detect loss by frame-id gaps and ask for a fresh keyframe.
                    if let Some(prev) = last_frame_id {
                        if frame.frame_id != prev.wrapping_add(1) && !frame.keyframe {
                            keyframe_requests += 1;
                            let _ = tx.send(ControlMessage::RequestKeyframe).await;
                        }
                    }
                    last_frame_id = Some(frame.frame_id);

                    frames += 1;
                    bytes += frame.data.len() as u64;
                    decoded += decoder
                        .decode(&frame.data, frame.keyframe, frame.timestamp_ms)?
                        .len() as u64;

                    if args.log_every > 0 && frames % args.log_every == 0 {
                        let secs = window_start.elapsed().as_secs_f64().max(1e-6);
                        let mbps = (bytes as f64 * 8.0) / secs / 1_000_000.0;
                        tracing::info!(
                            "{frames} frames, {:.1} fps, {mbps:.1} Mbit/s, {} decoded, {} keyframe requests, {} dropped",
                            frames as f64 / secs,
                            decoded,
                            keyframe_requests,
                            video.dropped(),
                        );
                        window_start = Instant::now();
                        frames = 0;
                        bytes = 0;
                    }
                }
            }

            _ = heartbeat.tick() => {
                if last_rx.elapsed() > HEARTBEAT_TIMEOUT {
                    bail!("control heartbeat timed out after {:?}", last_rx.elapsed());
                }
                nonce += 1;
                tx.send(ControlMessage::Ping { nonce }).await?;
            }
        }
    }
}
