# macscreenshare wire protocol (v1)

Single source of truth for the host (Swift, macOS) and client (Rust, Windows/macOS).

## 1. Transport

Two channels, both over the LAN:

| Channel | Protocol | Default port | Purpose |
|---|---|---|---|
| Control | TCP | **47810** | handshake, input, clipboard, heartbeat, stats |
| Video | UDP | **47811** | encoded video frames |

The host learns the client's UDP address from the `start_stream` control message
(peer IP from the TCP socket + client-supplied `udp_port`). No separate UDP
handshake is required.

All multi-byte integers are **big-endian** unless stated otherwise.

## 2. Control framing

Every control message is a length-prefixed frame:

```
+--------------------+-------------------------------+
| u32 length         | length bytes of JSON (UTF-8)  |
+--------------------+-------------------------------+
```

`length` is the byte length of the JSON payload and MUST be `<= 8 MiB`
(oversized frames are a protocol error and the connection is closed).

### 2.1 Messages

Messages are a serde/Swift `Codable` tagged enum. The tag field is `type`
(snake_case).

| `type` | Direction | Payload |
|---|---|---|
| `hello` | client → host | `protocol`, `client_name`, `token?` |
| `hello_ack` | host → client | `protocol`, `session_id`, `host_name`, `display_width`, `display_height`, `scale`, `video_port`, `codecs`, `capabilities?` |
| `error` | host → client | `code`, `message` |
| `ping` | both | `nonce` |
| `pong` | both | `nonce` |
| `start_stream` | client → host | `codec`, `fps`, `max_bitrate_kbps`, `udp_port` |
| `stream_started` | host → client | `codec`, `width`, `height`, `fps` |
| `stop_stream` | client → host | – |
| `input` | client → host | one of the input events below |
| `clipboard` | both | `text` |
| `request_keyframe` | client → host | – |
| `set_bitrate` | client → host | `max_bitrate_kbps` |
| `stats` | host → client | `fps`, `bitrate_kbps`, `encode_ms`, `dropped` |
| `bye` | both | – |

`protocol` is an integer; this document describes **1**. A mismatch is rejected
with `error { code: "protocol_mismatch" }`.

`scale` is the host display's backing scale factor (2.0 on Retina). Clients use
it for window sizing hints only; coordinates are **always normalized** (below).

`codecs` is a list of supported codec ids, most-preferred first.

`capabilities` is an optional list of host features. It is additive: hosts that
predate it omit the field and clients MUST treat its absence as "no optional
features". Unknown capability strings are ignored. This document defines
`adaptive_bitrate` (see §2.3).

### 2.2 Input events

Nested under `input` as `event`:

```json
{ "type": "input", "event": { "kind": "mouse_move", "x": 0.42, "y": 0.13 } }
```

| `kind` | Fields |
|---|---|
| `mouse_move` | `x`, `y` |
| `mouse_button` | `button` (`left`/`right`/`middle`/`back`/`forward`), `down` (bool), `x`, `y` |
| `mouse_scroll` | `dx`, `dy` (positive = right/down; units are "ticks", fractional allowed) |
| `key` | `code` (HID usage id, u16), `down` (bool), `modifiers` (u8 bitfield) |

`x`/`y` are `f32` in `[0.0, 1.0]` relative to the host display, independent of
Retina scaling, DPI, window size, or letterboxing. The client must convert from
window pixels to normalized space *after* accounting for aspect-ratio padding.

Modifiers bitfield: `shift=1`, `ctrl=2`, `alt=4`, `meta=8`.

Key codes are **USB HID usage ids** (usage page 0x07), e.g. `0x04` = `a`,
`0x28` = Return, `0xE0` = Left Ctrl. This makes the mapping portable between
Windows scancodes/VKs, `winit::KeyCode`, and macOS `CGKeyCode`.

### 2.3 Adaptive bitrate

`set_bitrate` changes the encoder's target bitrate mid-stream. It lets the client
react to loss without tearing down the stream.

- A client MUST only send `set_bitrate` if the host advertised
  `adaptive_bitrate` in `hello_ack.capabilities`. Hosts that did not advertise it
  will not understand the message, so clients that ignore this rule may be
  disconnected with `error { code: "bad_request" }`.
- `max_bitrate_kbps` is a target, clamped by the host to a sane range (currently
  `500..200000`). It does not by itself force a keyframe.
- The client derives the target from its own loss measurement (frame-id gaps
  across the UDP stream), starting at the `max_bitrate_kbps` it sent in
  `start_stream` and using additive-increase / multiplicative-decrease. It
  should apply hysteresis so it does not send changes for noise.
- Neither side is required to implement adaptive bitrate. A host that never
  advertises the capability and a client that never sends the message behave
  exactly as in fixed-rate v1.

## 3. Video framing

Each pulse is an independent datagram. Every datagram starts with a fixed
**16-byte header**:

```
offset  size  field
0       4     frame_id      monotonically increasing, wraps at u32
4       2     chunk_index   0-based
6       2     chunk_count   total chunks for this frame (>= 1)
8       1     flags         bit0 = keyframe, bit1 = last_chunk
9       1     codec         0 = H.264 Annex-B, 1 = HEVC Annex-B
10      2     reserved      must be 0
12      4     timestamp_ms  host monotonic clock, milliseconds
```

Payload follows the header. The payload of chunk 0 for a keyframe begins with
the codec's parameter sets (H.264: SPS+PPS+IDR NALs) so a client can start
decoding without a separate config exchange.

Constraints:

- Max datagram size (header + payload): **1400 bytes** to stay under typical
  Wi-Fi MTU without IP fragmentation. Payload chunk size is therefore 1384.
- A frame with `chunk_count == 1` has `last_chunk` set.
- `frame_id` gaps mean loss. The client requests recovery by sending
  `request_keyframe`; it does **not** ask for retransmission in v1.

### 3.1 Reassembly

The client buffers chunks by `frame_id` and emits a frame when all chunks are
present or when a newer `frame_id` arrives (dropping the incomplete one).
An incomplete frame older than ~200 ms is discarded to bound memory.

## 4. Lifecycle

```
client                                host
  |---- TCP connect ------------------->|
  |---- hello ------------------------->|
  |<--- hello_ack ---------------------|
  |---- ping/pong (every 2s) ---------->|
  |---- start_stream {udp_port} ------->|
  |<--- stream_started -----------------|
  |<=== UDP video datagrams ============|
  |---- input ... --------------------->|
  |<--- stats (every 1s) ---------------|
  |---- stop_stream / bye ------------->|
```

Heartbeat: if no control traffic is observed for **10 s** (5 missed pings) either
side closes the connection. The client then retries with exponential backoff
(0.5 s → 8 s cap).

## 5. Security model (v1)

Targeted at a **trusted LAN**. The `token` in `hello` is an optional shared
secret set on the host; when present it must match or the host replies
`error { code: "unauthorized" }`. Traffic is not encrypted in v1; layering
TLS/QUIC is planned but out of scope.

## 6. Versioning

`protocol` in `hello`/`hello_ack` is bumped on any breaking change. Additive
optional fields (with defaults) do not bump the version.
