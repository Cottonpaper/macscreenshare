//! Control-channel framing and UDP video plumbing.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use macss_protocol::{
    ControlMessage, FrameReassembler, VideoFrame, MAX_CONTROL_FRAME_LEN, MAX_DATAGRAM_LEN,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UdpSocket;

/// Write one length-prefixed control message.
pub async fn write_message<W>(w: &mut W, msg: &ControlMessage) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(msg).context("serialize control message")?;
    if payload.len() > MAX_CONTROL_FRAME_LEN {
        bail!("control message too large: {} bytes", payload.len());
    }
    w.write_all(&(payload.len() as u32).to_be_bytes()).await?;
    w.write_all(&payload).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed control message. Returns `Ok(None)` on a clean EOF.
pub async fn read_message<R>(r: &mut R) -> Result<Option<ControlMessage>>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e).context("read control frame length"),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None);
    }
    if len > MAX_CONTROL_FRAME_LEN {
        bail!("control frame too large: {len} bytes");
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)
        .await
        .context("read control frame body")?;
    let msg = serde_json::from_slice(&payload).context("deserialize control message")?;
    Ok(Some(msg))
}

/// Sends encoded frames over UDP to a fixed peer.
pub struct VideoSender {
    socket: UdpSocket,
    peer: SocketAddr,
    frames_sent: u64,
    bytes_sent: u64,
}

impl VideoSender {
    pub async fn connect(peer: SocketAddr) -> Result<Self> {
        let bind: SocketAddr = if peer.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let socket = UdpSocket::bind(bind).await.context("bind video sender")?;
        Ok(Self {
            socket,
            peer,
            frames_sent: 0,
            bytes_sent: 0,
        })
    }

    /// Send a frame already split into datagrams by [`macss_protocol::packetize_frame`].
    pub async fn send_datagrams(&mut self, datagrams: &[Vec<u8>]) -> Result<()> {
        for d in datagrams {
            let n = self.socket.send_to(d, self.peer).await.context("send video")?;
            self.bytes_sent += n as u64;
        }
        self.frames_sent += 1;
        Ok(())
    }

    pub fn stats(&self) -> (u64, u64) {
        (self.frames_sent, self.bytes_sent)
    }
}

/// Receives video datagrams and reassembles them into frames.
pub struct VideoReceiver {
    socket: UdpSocket,
    reassembler: FrameReassembler,
    buf: Vec<u8>,
    dropped: u64,
    last_activity: Instant,
}

impl VideoReceiver {
    /// Bind to `port` on all interfaces.
    pub async fn bind(port: u16) -> Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", port))
            .await
            .with_context(|| format!("bind video receiver on udp/{port}"))?;
        Ok(Self {
            socket,
            reassembler: FrameReassembler::new(),
            buf: vec![0u8; MAX_DATAGRAM_LEN],
            dropped: 0,
            last_activity: Instant::now(),
        })
    }

    /// The port actually bound (useful when `port == 0`).
    pub fn local_port(&self) -> Result<u16> {
        Ok(self.socket.local_addr()?.port())
    }

    /// Await the next complete frame, or `None` after `timeout` of silence.
    pub async fn recv_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let (n, _from) = match tokio::time::timeout(remaining, self.socket.recv_from(&mut self.buf))
                .await
            {
                Ok(res) => res.context("recv video")?,
                Err(_) => return Ok(None),
            };
            self.last_activity = Instant::now();
            if let Some(frame) = self.reassembler.push(&self.buf[..n]) {
                return Ok(Some(frame));
            }
            // Incomplete frame: a newer one superseded it.
            if self.reassembler.pending_len() == 0 {
                self.dropped += 1;
            }
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn reset(&mut self) {
        self.reassembler.clear();
    }
}

/// Exponential backoff used by the client when reconnecting.
#[derive(Debug, Clone)]
pub struct Backoff {
    current: Duration,
    max: Duration,
}

impl Backoff {
    pub fn new(initial: Duration, max: Duration) -> Self {
        Self {
            current: initial,
            max,
        }
    }

    /// Return the next delay and advance the state.
    pub fn next_delay(&mut self) -> Duration {
        let d = self.current;
        self.current = (self.current * 2).min(self.max);
        d
    }

    pub fn reset(&mut self) {
        self.current = self.max.min(Duration::from_millis(500));
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(500), Duration::from_secs(8))
    }
}

/// Convenience: parse a `host:port` string, filling in the default control port.
pub fn parse_host_port(s: &str, default_port: u16) -> Result<SocketAddr> {
    use std::net::ToSocketAddrs;
    let with_port = if s.contains(':') {
        s.to_string()
    } else {
        format!("{s}:{default_port}")
    };
    let mut addrs = with_port
        .to_socket_addrs()
        .with_context(|| format!("resolve {with_port}"))?;
    addrs
        .next()
        .ok_or_else(|| anyhow!("no address for {with_port}"))
}

/// Send a Wake-on-LAN magic packet for `mac` to the LAN broadcast address.
pub async fn send_magic_packet(mac: [u8; 6], port: u16) -> Result<()> {
    let mut packet = Vec::with_capacity(102);
    packet.extend_from_slice(&[0xFFu8; 6]);
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.set_broadcast(true)?;
    let target = SocketAddr::from(([255, 255, 255, 255], port));
    socket.send_to(&packet, target).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use macss_protocol::{packetize_frame, Codec, InputEvent, PROTOCOL_VERSION};
    use tokio::io::duplex;

    #[tokio::test]
    async fn control_roundtrip_over_duplex() {
        let (mut a, mut b) = duplex(64 * 1024);
        let msgs = vec![
            ControlMessage::Hello {
                protocol: PROTOCOL_VERSION,
                client_name: "test".into(),
                token: None,
            },
            ControlMessage::Input {
                event: InputEvent::MouseMove { x: 0.1, y: 0.9 },
            },
            ControlMessage::Bye,
        ];
        for m in msgs.clone() {
            write_message(&mut a, &m).await.unwrap();
        }
        for m in msgs {
            let got = read_message(&mut b).await.unwrap().unwrap();
            assert_eq!(got, m);
        }
    }

    #[tokio::test]
    async fn read_message_returns_none_on_eof() {
        let (a, mut b) = duplex(1024);
        drop(a);
        assert!(read_message(&mut b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn video_send_recv_roundtrip() {
        let mut rx = VideoReceiver::bind(0).await.unwrap();
        let port = rx.local_port().unwrap();
        let mut tx = VideoSender::connect(([127, 0, 0, 1], port).into())
            .await
            .unwrap();

        let payload: Vec<u8> = (0..8000u32).map(|i| (i % 200) as u8).collect();
        let datagrams = packetize_frame(1, Codec::H264, true, 42, &payload);
        tx.send_datagrams(&datagrams).await.unwrap();

        let frame = rx
            .recv_frame(Duration::from_secs(1))
            .await
            .unwrap()
            .expect("frame");
        assert_eq!(frame.frame_id, 1);
        assert!(frame.keyframe);
        assert_eq!(frame.data, payload);
    }

    #[test]
    fn backoff_grows_and_resets() {
        let mut b = Backoff::new(Duration::from_millis(500), Duration::from_secs(8));
        assert_eq!(b.next_delay(), Duration::from_millis(500));
        assert_eq!(b.next_delay(), Duration::from_millis(1000));
        assert_eq!(b.next_delay(), Duration::from_millis(2000));
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_millis(500));
    }

    #[test]
    fn parse_host_port_defaults() {
        assert_eq!(
            parse_host_port("127.0.0.1", 47810).unwrap(),
            "127.0.0.1:47810".parse().unwrap()
        );
        assert_eq!(
            parse_host_port("127.0.0.1:9000", 47810).unwrap(),
            "127.0.0.1:9000".parse().unwrap()
        );
    }
}
