//! Client session: handshake, stream setup, receive loop, heartbeat.
//!
//! The same code drives the GUI viewer and the headless mode; the difference is
//! only whether [`SessionHooks`] carries a frame sink and an input receiver.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use macss_decoder::{
    backend_name, default_decoder, synthetic::SyntheticSource, FrameSink, VideoDecoder,
};
use macss_protocol::{ControlMessage, InputEvent, PROTOCOL_VERSION};
use macss_transport::{
    parse_host_port, read_message, send_magic_packet, write_message, Backoff, VideoReceiver,
};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::task::JoinHandle;

use crate::args::Args;
use crate::bitrate::{BitrateConfig, BitrateController};

const HEARTBEAT: Duration = Duration::from_secs(2);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_POLL: Duration = Duration::from_millis(250);

/// Endpoints the session reads from and writes to.
#[derive(Default)]
pub struct SessionHooks {
    /// Where decoded frames are delivered (the renderer).
    pub frames: Option<Arc<dyn FrameSink>>,
    /// Input events produced by the client UI.
    pub input: Option<UnboundedReceiver<InputEvent>>,
}

/// Connect, reconnect on failure, and never return until interrupted.
pub async fn run_forever(args: &Args, hooks: &mut SessionHooks) -> Result<()> {
    if let Some(mac) = args.wol {
        match send_magic_packet(mac, args.wol_port).await {
            Ok(()) => tracing::info!("sent Wake-on-LAN magic packet to {mac:02x?}"),
            Err(error) => tracing::warn!("Wake-on-LAN failed: {error:#}"),
        }
        // Give the host a moment to wake before dialling.
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let mut backoff = Backoff::default();
    loop {
        match run_once(args, hooks).await {
            Ok(()) => backoff.reset(),
            Err(error) => tracing::warn!("session ended: {error:#}"),
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

/// Connect once and run until the connection ends. Returns `Ok(())` when the
/// host closes cleanly so the caller can reconnect.
pub async fn run_once(args: &Args, hooks: &mut SessionHooks) -> Result<()> {
    let addr = parse_host_port(&args.host, args.control_port)?;
    tracing::info!("connecting to {addr}");
    let stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect to {addr}"))?;
    stream.set_nodelay(true).ok();
    let (mut rd, mut wr) = stream.into_split();

    let (tx, mut rx) = mpsc::channel::<ControlMessage>(256);
    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if let Err(error) = write_message(&mut wr, &message).await {
                tracing::debug!("control write failed: {error:#}");
                break;
            }
        }
    });

    let result = handshake_and_stream(args, &mut rd, &tx, hooks).await;

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
    hooks: &mut SessionHooks,
) -> Result<()> {
    tx.send(ControlMessage::Hello {
        protocol: PROTOCOL_VERSION,
        client_name: args.client_name.clone(),
        token: args.token.clone(),
    })
    .await?;

    let (display_width, display_height, codecs, capabilities) = match read_message(rd).await? {
        Some(ControlMessage::HelloAck {
            protocol,
            session_id,
            host_name,
            display_width,
            display_height,
            scale,
            video_port: _,
            codecs,
            capabilities,
        }) => {
            if protocol != PROTOCOL_VERSION {
                bail!("host speaks protocol {protocol}, we speak {PROTOCOL_VERSION}");
            }
            tracing::info!(
                "connected to {host_name} (session {session_id}) display {display_width}x{display_height} @{scale}x"
            );
            (display_width, display_height, codecs, capabilities)
        }
        Some(ControlMessage::Error { code, message }) => {
            bail!("host rejected connection: {code}: {message}")
        }
        Some(other) => bail!("unexpected message during handshake: {other:?}"),
        None => bail!("host closed the connection during handshake"),
    };

    // Adaptive bitrate needs the host to honour `set_bitrate`; otherwise we
    // stay at the requested fixed rate.
    let adaptive = !args.no_adaptive_bitrate
        && capabilities
            .iter()
            .any(|c| c == macss_protocol::capabilities::ADAPTIVE_BITRATE);
    if adaptive {
        tracing::info!("host supports adaptive bitrate");
    }

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
    tracing::info!(
        "requested {codec:?} @ {} fps, <= {} kbit/s, udp/{udp_port}",
        args.fps,
        args.bitrate_kbps
    );

    let mut decoder = default_decoder().context("create decoder")?;
    tracing::info!("decoder: {}", backend_name());

    // Optional synthetic frames let the render path be exercised with no decoder.
    let synthetic = if args.synthetic_video {
        match hooks.frames.clone() {
            Some(sink) => {
                tracing::info!(
                    "synthetic video: generating {display_width}x{display_height} test frames"
                );
                Some(spawn_synthetic(sink, display_width, display_height, args.fps))
            }
            None => {
                tracing::warn!("--synthetic-video has no effect without a renderer");
                None
            }
        }
    } else {
        None
    };
    let submit_decoded = synthetic.is_none();

    let result = receive_loop(
        args,
        rd,
        tx,
        hooks,
        &mut video,
        decoder.as_mut(),
        submit_decoded,
        adaptive,
    )
    .await;

    if let Some(handle) = synthetic {
        handle.abort();
    }
    result
}

fn spawn_synthetic(
    sink: Arc<dyn FrameSink>,
    width: u32,
    height: u32,
    fps: u32,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut source = SyntheticSource::new(width.max(2), height.max(2));
        let period = Duration::from_millis((1000 / fps.max(1) as u64).max(1));
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            sink.submit(source.next_frame());
        }
    })
}

/// Await the next UI input event. Never resolves when there is no UI, or once
/// the UI channel has closed, so the select branch stays dormant.
async fn next_input(hooks: &mut SessionHooks) -> InputEvent {
    match hooks.input.as_mut() {
        Some(receiver) => loop {
            match receiver.recv().await {
                Some(event) => return event,
                None => std::future::pending::<()>().await,
            }
        },
        None => std::future::pending().await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn receive_loop(
    args: &Args,
    rd: &mut tokio::net::tcp::OwnedReadHalf,
    tx: &mpsc::Sender<ControlMessage>,
    hooks: &mut SessionHooks,
    video: &mut VideoReceiver,
    decoder: &mut dyn VideoDecoder,
    submit_decoded: bool,
    adaptive: bool,
) -> Result<()> {
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut nonce: u64 = 0;
    let mut last_rx = Instant::now();

    // Adaptive bitrate: one measurement window per second.
    let mut bitrate = BitrateController::new(BitrateConfig::for_cap(args.bitrate_kbps));
    let mut bitrate_tick = tokio::time::interval(Duration::from_secs(1));
    bitrate_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut expected_since: u64 = 0;
    let mut lost_since: u64 = 0;

    let mut frames: u64 = 0;
    let mut bytes: u64 = 0;
    let mut decoded: u64 = 0;
    let mut last_frame_id: Option<u32> = None;
    let mut keyframe_requests: u64 = 0;
    let mut window_start = Instant::now();

    loop {
        tokio::select! {
            message = read_message(rd) => {
                last_rx = Instant::now();
                match message? {
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

            event = next_input(hooks) => {
                tx.send(ControlMessage::Input { event }).await?;
            }

            frame = video.recv_frame(READ_POLL) => {
                last_rx = Instant::now();
                if let Some(frame) = frame? {
                    // Detect loss by frame-id gaps and ask for a fresh keyframe.
                    if let Some(previous) = last_frame_id {
                        // Clamp so a wrap or long stall can't skew the loss ratio.
                        let gap = (frame.frame_id.wrapping_sub(previous).saturating_sub(1) as u64)
                            .min(10_000);
                        lost_since += gap;
                        expected_since += gap + 1;
                        if gap > 0 && !frame.keyframe {
                            keyframe_requests += 1;
                            let _ = tx.send(ControlMessage::RequestKeyframe).await;
                        }
                    }
                    last_frame_id = Some(frame.frame_id);

                    frames += 1;
                    bytes += frame.data.len() as u64;
                    let decoded_frames =
                        decoder.decode(&frame.data, frame.keyframe, frame.timestamp_ms)?;
                    decoded += decoded_frames.len() as u64;
                    if submit_decoded {
                        if let Some(sink) = hooks.frames.as_ref() {
                            for decoded_frame in decoded_frames {
                                sink.submit(decoded_frame);
                            }
                        }
                    }

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

            // Adaptive bitrate: evaluate one window per second. Disabled (no-op)
            // unless the host advertised support and the user kept it on.
            _ = bitrate_tick.tick(), if adaptive => {
                if let Some(kbps) = bitrate.observe(expected_since, lost_since) {
                    tracing::info!("adaptive bitrate -> {kbps} kbit/s");
                    let _ = tx
                        .send(ControlMessage::SetBitrate { max_bitrate_kbps: kbps })
                        .await;
                }
                expected_since = 0;
                lost_since = 0;
            }
        }
    }
}
