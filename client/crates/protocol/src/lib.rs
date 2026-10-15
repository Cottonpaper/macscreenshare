//! Wire protocol types shared by the macscreenshare host and client.
//!
//! See `docs/protocol.md` at the repository root for the normative spec.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Protocol version carried in `hello` / `hello_ack`.
pub const PROTOCOL_VERSION: u16 = 1;

/// Default TCP control port.
pub const DEFAULT_CONTROL_PORT: u16 = 47810;
/// Default UDP video port.
pub const DEFAULT_VIDEO_PORT: u16 = 47811;

/// Fixed size of the video datagram header.
pub const VIDEO_HEADER_LEN: usize = 16;
/// Payload bytes per video datagram chunk.
pub const VIDEO_CHUNK_LEN: usize = 1384;
/// Maximum on-the-wire datagram size (header + payload).
pub const MAX_DATAGRAM_LEN: usize = VIDEO_HEADER_LEN + VIDEO_CHUNK_LEN;
/// Maximum accepted control frame payload.
pub const MAX_CONTROL_FRAME_LEN: usize = 8 * 1024 * 1024;

/// Bit flags in the video header `flags` byte.
pub mod video_flags {
    pub const KEYFRAME: u8 = 1 << 0;
    pub const LAST_CHUNK: u8 = 1 << 1;
}

/// Bit values for the `modifiers` field of key events.
pub mod modifiers {
    pub const SHIFT: u8 = 1 << 0;
    pub const CTRL: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const META: u8 = 1 << 3;
}

/// Optional host capabilities advertised in `hello_ack`.
pub mod capabilities {
    /// Host honours `set_bitrate` mid-stream.
    pub const ADAPTIVE_BITRATE: &str = "adaptive_bitrate";
}

/// Codec identifiers used on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    /// H.264 in Annex-B framing (id 0).
    H264,
    /// HEVC in Annex-B framing (id 1).
    Hevc,
}

impl Codec {
    pub fn to_wire(self) -> u8 {
        match self {
            Codec::H264 => 0,
            Codec::Hevc => 1,
        }
    }

    pub fn from_wire(v: u8) -> Option<Self> {
        match v {
            0 => Some(Codec::H264),
            1 => Some(Codec::Hevc),
            _ => None,
        }
    }
}

/// Mouse buttons understood by the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// A single input event sent from client to host.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputEvent {
    /// Absolute normalized position in `[0, 1]` on the host display.
    MouseMove { x: f32, y: f32 },
    /// Button press/release at an absolute normalized position.
    MouseButton {
        button: Button,
        down: bool,
        x: f32,
        y: f32,
    },
    /// Scroll. `dx`/`dy` are ticks; positive is right/down.
    MouseScroll { dx: f32, dy: f32 },
    /// Key press/release. `code` is a USB HID usage id.
    Key {
        code: u16,
        down: bool,
        modifiers: u8,
    },
}

/// Control-channel message. Internally tagged on the `type` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    Hello {
        protocol: u16,
        client_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    HelloAck {
        protocol: u16,
        session_id: String,
        host_name: String,
        display_width: u32,
        display_height: u32,
        scale: f32,
        video_port: u16,
        codecs: Vec<Codec>,
        /// Additive capability list; absent on hosts that predate it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },
    Error {
        code: String,
        message: String,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    StartStream {
        codec: Codec,
        fps: u32,
        max_bitrate_kbps: u32,
        udp_port: u16,
    },
    StreamStarted {
        codec: Codec,
        width: u32,
        height: u32,
        fps: u32,
    },
    StopStream,
    Input {
        event: InputEvent,
    },
    Clipboard {
        text: String,
    },
    RequestKeyframe,
    /// Change the encoder target bitrate mid-stream. Only sent when the host
    /// advertised [`capabilities::ADAPTIVE_BITRATE`].
    SetBitrate {
        max_bitrate_kbps: u32,
    },
    Stats {
        fps: f32,
        bitrate_kbps: u32,
        encode_ms: f32,
        dropped: u32,
    },
    Bye,
}

/// Well-known error codes used in [`ControlMessage::Error`].
pub mod error_codes {
    pub const PROTOCOL_MISMATCH: &str = "protocol_mismatch";
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const BUSY: &str = "busy";
    pub const BAD_REQUEST: &str = "bad_request";
}

/// Parsed video datagram header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    pub frame_id: u32,
    pub chunk_index: u16,
    pub chunk_count: u16,
    pub flags: u8,
    pub codec: u8,
    pub timestamp_ms: u32,
}

impl VideoHeader {
    pub fn is_keyframe(&self) -> bool {
        self.flags & video_flags::KEYFRAME != 0
    }

    pub fn is_last_chunk(&self) -> bool {
        self.flags & video_flags::LAST_CHUNK != 0
    }

    pub fn encode(&self) -> [u8; VIDEO_HEADER_LEN] {
        let mut buf = [0u8; VIDEO_HEADER_LEN];
        buf[0..4].copy_from_slice(&self.frame_id.to_be_bytes());
        buf[4..6].copy_from_slice(&self.chunk_index.to_be_bytes());
        buf[6..8].copy_from_slice(&self.chunk_count.to_be_bytes());
        buf[8] = self.flags;
        buf[9] = self.codec;
        // buf[10..12] reserved
        buf[12..16].copy_from_slice(&self.timestamp_ms.to_be_bytes());
        buf
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < VIDEO_HEADER_LEN {
            return None;
        }
        Some(VideoHeader {
            frame_id: u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
            chunk_index: u16::from_be_bytes([buf[4], buf[5]]),
            chunk_count: u16::from_be_bytes([buf[6], buf[7]]),
            flags: buf[8],
            codec: buf[9],
            timestamp_ms: u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]),
        })
    }
}

/// Split an encoded frame into datagram-sized chunks ready to send.
///
/// Each returned buffer already includes the 16-byte header.
pub fn packetize_frame(
    frame_id: u32,
    codec: Codec,
    keyframe: bool,
    timestamp_ms: u32,
    payload: &[u8],
) -> Vec<Vec<u8>> {
    let count = payload.len().div_ceil(VIDEO_CHUNK_LEN).max(1);
    let count_u16 = count.min(u16::MAX as usize) as u16;
    let mut out = Vec::with_capacity(count);
    for (i, chunk) in payload.chunks(VIDEO_CHUNK_LEN).enumerate() {
        let mut flags = 0u8;
        if keyframe && i == 0 {
            flags |= video_flags::KEYFRAME;
        }
        if i + 1 == count {
            flags |= video_flags::LAST_CHUNK;
        }
        let header = VideoHeader {
            frame_id,
            chunk_index: i as u16,
            chunk_count: count_u16,
            flags,
            codec: codec.to_wire(),
            timestamp_ms,
        };
        let mut datagram = Vec::with_capacity(VIDEO_HEADER_LEN + chunk.len());
        datagram.extend_from_slice(&header.encode());
        datagram.extend_from_slice(chunk);
        out.push(datagram);
    }
    out
}

/// A reassembled video frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub frame_id: u32,
    pub codec: Codec,
    pub keyframe: bool,
    pub timestamp_ms: u32,
    pub data: Vec<u8>,
}

#[derive(Debug)]
struct PartialFrame {
    codec: Codec,
    keyframe: bool,
    timestamp_ms: u32,
    chunk_count: u16,
    received: u16,
    /// Filled lazily once `chunk_count` is known.
    chunks: Vec<Option<Vec<u8>>>,
}

/// Reassembles video datagrams into complete frames.
#[derive(Debug, Default)]
pub struct FrameReassembler {
    pending: BTreeMap<u32, PartialFrame>,
    last_emitted: Option<u32>,
}

/// Returns true when `a` is newer than `b` under wrapping comparison.
fn is_newer(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) != 0 && a.wrapping_sub(b) < u32::MAX / 2
}

impl FrameReassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one datagram. Returns a frame when it completes.
    pub fn push(&mut self, datagram: &[u8]) -> Option<VideoFrame> {
        let header = VideoHeader::decode(datagram)?;
        let payload = &datagram[VIDEO_HEADER_LEN..];

        // Drop anything strictly older than the last frame we emitted.
        if let Some(last) = self.last_emitted {
            if !is_newer(header.frame_id, last) {
                return None;
            }
        }
        // Any smaller pending frame id is now obsolete.
        if let Some(last) = self.last_emitted {
            self.pending.retain(|id, _| is_newer(*id, last));
        }

        // Detect a frame we can no longer complete (a newer one appeared).
        let newest = self.pending.keys().next_back().copied();
        if let Some(newest_id) = newest {
            if is_newer(header.frame_id, newest_id) {
                self.pending.retain(|id, _| *id == header.frame_id);
            }
        }

        let codec = Codec::from_wire(header.codec).unwrap_or(Codec::H264);
        let entry = self.pending.entry(header.frame_id).or_insert_with(|| PartialFrame {
            codec,
            keyframe: header.is_keyframe(),
            timestamp_ms: header.timestamp_ms,
            chunk_count: header.chunk_count,
            received: 0,
            chunks: vec![None; header.chunk_count.max(1) as usize],
        });

        let idx = header.chunk_index as usize;
        if idx < entry.chunks.len() && entry.chunks[idx].is_none() {
            entry.chunks[idx] = Some(payload.to_vec());
            entry.received += 1;
        }

        if entry.received == entry.chunk_count {
            let done = self.pending.remove(&header.frame_id).unwrap();
            let cap: usize = done
                .chunks
                .iter()
                .filter_map(|c| c.as_ref())
                .map(|c| c.len())
                .sum();
            let mut data = Vec::with_capacity(cap);
            for chunk in done.chunks.into_iter().flatten() {
                data.extend_from_slice(&chunk);
            }
            self.last_emitted = Some(header.frame_id);
            return Some(VideoFrame {
                frame_id: header.frame_id,
                codec: done.codec,
                keyframe: done.keyframe,
                timestamp_ms: done.timestamp_ms,
                data,
            });
        }
        None
    }

    /// Number of frames currently being reassembled.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Discard all in-flight frames (e.g. after a connection reset).
    pub fn clear(&mut self) {
        self.pending.clear();
        self.last_emitted = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = VideoHeader {
            frame_id: 0xDEAD_BEEF,
            chunk_index: 3,
            chunk_count: 7,
            flags: video_flags::KEYFRAME,
            codec: 1,
            timestamp_ms: 1234,
        };
        let enc = h.encode();
        assert_eq!(enc.len(), VIDEO_HEADER_LEN);
        assert_eq!(VideoHeader::decode(&enc), Some(h));
        assert!(h.is_keyframe());
        assert!(!h.is_last_chunk());
    }

    #[test]
    fn header_rejects_short() {
        assert_eq!(VideoHeader::decode(&[0u8; 4]), None);
    }

    #[test]
    fn packetize_small_frame_single_chunk() {
        let datagrams = packetize_frame(1, Codec::H264, true, 10, b"hello");
        assert_eq!(datagrams.len(), 1);
        let h = VideoHeader::decode(&datagrams[0]).unwrap();
        assert_eq!(h.chunk_count, 1);
        assert!(h.is_keyframe());
        assert!(h.is_last_chunk());
        assert_eq!(&datagrams[0][VIDEO_HEADER_LEN..], b"hello");
    }

    #[test]
    fn packetize_reassembles_in_order() {
        let payload: Vec<u8> = (0..10_000).map(|i| (i % 251) as u8).collect();
        let datagrams = packetize_frame(42, Codec::H264, true, 99, &payload);
        assert!(datagrams.len() > 1);
        for d in &datagrams {
            assert!(d.len() <= MAX_DATAGRAM_LEN);
        }

        let mut re = FrameReassembler::new();
        let mut result = None;
        for d in &datagrams {
            result = re.push(d);
        }
        let frame = result.expect("frame should complete");
        assert_eq!(frame.frame_id, 42);
        assert!(frame.keyframe);
        assert_eq!(frame.data, payload);
    }

    #[test]
    fn reassembler_handles_out_of_order_chunks() {
        let payload: Vec<u8> = (0..5000u32).map(|i| (i % 97) as u8).collect();
        let mut datagrams = packetize_frame(7, Codec::H264, false, 1, &payload);
        // shuffle by reversing chunk order (not byte order)
        datagrams.reverse();

        let mut re = FrameReassembler::new();
        let mut result = None;
        for d in &datagrams {
            if let Some(f) = re.push(d) {
                result = Some(f);
            }
        }
        assert_eq!(result.unwrap().data, payload);
    }

    #[test]
    fn reassembler_drops_incomplete_frame_when_newer_arrives() {
        let big: Vec<u8> = vec![1u8; 5000];
        let nxt: Vec<u8> = vec![2u8; 5000];
        let mut big_dg = packetize_frame(1, Codec::H264, true, 0, &big);
        let next_dg = packetize_frame(2, Codec::H264, false, 1, &nxt);
        big_dg.pop(); // drop the final chunk -> incomplete

        let mut re = FrameReassembler::new();
        for d in &big_dg {
            assert!(re.push(d).is_none());
        }
        assert_eq!(re.pending_len(), 1);

        // Feeding the newer frame's chunk 0 evicts frame 1.
        let first = re.push(&next_dg[0]);
        assert!(first.is_none());
        assert_eq!(re.pending_len(), 1);
    }

    #[test]
    fn reassembler_ignores_stale_frames() {
        let payload: Vec<u8> = vec![9u8; 100];
        let mut re = FrameReassembler::new();
        for d in packetize_frame(5, Codec::H264, true, 0, &payload) {
            assert!(re.push(&d).is_some());
        }
        // A late retransmission of frame 4 must be ignored.
        for d in packetize_frame(4, Codec::H264, false, 0, &payload) {
            assert!(re.push(&d).is_none());
        }
    }

    #[test]
    fn control_message_json_roundtrip() {
        let msgs = vec![
            ControlMessage::Hello {
                protocol: PROTOCOL_VERSION,
                client_name: "desk".into(),
                token: Some("s3cret".into()),
            },
            ControlMessage::HelloAck {
                protocol: 1,
                session_id: "abc".into(),
                host_name: "studio".into(),
                display_width: 3024,
                display_height: 1964,
                scale: 2.0,
                video_port: DEFAULT_VIDEO_PORT,
                codecs: vec![Codec::H264, Codec::Hevc],
                capabilities: vec![capabilities::ADAPTIVE_BITRATE.to_string()],
            },
            ControlMessage::SetBitrate {
                max_bitrate_kbps: 8_000,
            },
            ControlMessage::Input {
                event: InputEvent::MouseMove { x: 0.5, y: 0.25 },
            },
            ControlMessage::Input {
                event: InputEvent::Key {
                    code: 0x04,
                    down: true,
                    modifiers: modifiers::SHIFT,
                },
            },
            ControlMessage::Error {
                code: error_codes::UNAUTHORIZED.into(),
                message: "bad token".into(),
            },
        ];

        for m in msgs {
            let json = serde_json::to_string(&m).unwrap();
            let back: ControlMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn tagged_enum_shape_is_stable() {
        let json = serde_json::to_value(ControlMessage::StopStream).unwrap();
        assert_eq!(json["type"], "stop_stream");

        let json = serde_json::to_value(ControlMessage::Input {
            event: InputEvent::MouseScroll { dx: 1.0, dy: -1.0 },
        })
        .unwrap();
        assert_eq!(json["type"], "input");
        assert_eq!(json["event"]["kind"], "mouse_scroll");
    }

    #[test]
    fn hello_ack_without_capabilities_defaults_empty() {
        let json = r#"{"type":"hello_ack","protocol":1,"session_id":"s","host_name":"h","display_width":100,"display_height":50,"scale":2.0,"video_port":47811,"codecs":["h264"]}"#;
        match serde_json::from_str::<ControlMessage>(json).unwrap() {
            ControlMessage::HelloAck { capabilities, .. } => assert!(capabilities.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn set_bitrate_tag_is_stable() {
        let json =
            serde_json::to_value(ControlMessage::SetBitrate { max_bitrate_kbps: 4000 }).unwrap();
        assert_eq!(json["type"], "set_bitrate");
        assert_eq!(json["max_bitrate_kbps"], 4000);
    }
}
