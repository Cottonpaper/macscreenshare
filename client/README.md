# macss-client (Rust)

Cross-platform client (Windows + macOS) for the macscreenshare protocol.
Current milestone (M0/M1): connect, handshake, request a stream, receive and
reassemble UDP video, report stats, and reconnect automatically. Rendering and
input capture are not wired up yet.

## Layout

| Crate | Purpose |
|---|---|
| `macss-protocol` | wire types, video framing, reassembly (no deps) |
| `macss-transport` | control framing, UDP video, backoff, Wake-on-LAN |
| `macss-decoder` | `VideoDecoder` trait, null decoder, optional FFmpeg backend |
| `macss-client` | the client binary + library (args, session) |
| `macss-mock-host` | a fake host for developing without a Mac |

## Build and test

```sh
cargo build --workspace
cargo test --workspace
```

## Try it without a Mac

`macss-mock-host` speaks the real protocol and streams synthetic frames.

```sh
# terminal 1
cargo run -p macss-mock-host -- --seconds 30 --fps 60

# terminal 2
cargo run -p macss-client -- 127.0.0.1 --log-every 60
```

## Running against a real host

```sh
cargo run -p macss-client -- <mac-host> --token <secret>
```

Useful flags: `--fps`, `--bitrate`, `--codec h264|hevc`, `--wol aa:bb:cc:dd:ee:ff`.

## Enabling real decoding (FFmpeg)

By default the client builds without FFmpeg: packets are counted but not
decoded, which is enough for protocol work. To decode for real:

```sh
cargo build -p macss-client --features macss-decoder/ffmpeg
```

This needs FFmpeg development libraries at build time:

- **Windows:** install via [vcpkg](https://vcpkg.io) — `vcpkg install ffmpeg:x64-windows`
  and set `VCPKG_ROOT`. `ffmpeg-next`'s `build` feature picks it up.
- **macOS:** `brew install ffmpeg pkg-config`.

The backend lives behind `macss_decoder::VideoDecoder`; a native
Media Foundation / VideoToolbox implementation can replace it later without
touching the transport.

## Verified in this environment

- `cargo test --workspace`: 24 tests (protocol framing/reassembly, control
  round-trips, UDP send/receive, backoff, arg parsing).
- `crates/client/tests/end_to_end.rs`: a mock host streams frames and the
  client session completes cleanly on `bye`.
