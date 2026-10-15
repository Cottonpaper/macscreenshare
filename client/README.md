# macss-client (Rust)

Cross-platform client (Windows + macOS) for the macscreenshare protocol.
Current milestone (M2): connect, handshake, receive and reassemble UDP video,
render it in a window, and forward mouse/keyboard back to the host, with
reconnect. Real H.264 decoding still needs the `ffmpeg` feature.

## Layout

| Crate | Purpose |
|---|---|
| `macss-protocol` | wire types, video framing, reassembly (no deps) |
| `macss-transport` | control framing, UDP video, backoff, Wake-on-LAN |
| `macss-decoder` | `VideoDecoder` trait, null/FFmpeg backends, synthetic frames |
| `macss-viewer` | winit + wgpu window, YUV renderer, input translation |
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
cargo run -p macss-mock-host -- --seconds 60 --fps 60

# terminal 2 — windowed, with local test frames so you can see something
cargo run -p macss-client -- 127.0.0.1 --synthetic-video
```

`--synthetic-video` draws a moving gradient locally, so you can validate the
whole receive → render → input path before installing FFmpeg or touching a Mac.

Useful flags:

| Flag | Meaning |
|---|---|
| `--headless` | No window; connect and log only |
| `--synthetic-video` | Generate local test frames instead of decoding |
| `--fps <N>` / `--bitrate <KBPS>` | Requested stream settings |
| `--codec h264\|hevc` | Preferred codec |
| `--token <T>` | Shared secret, if the host requires one |
| `--wol <MAC>` | Send a Wake-on-LAN magic packet before connecting |
| `--log-every <N>` | Log every N frames (0 disables) |

## Running against a real host

```sh
cargo run -p macss-client -- <mac-host> --token <secret>
```

Click the window to focus it; mouse and keyboard then drive the Mac. Video is
letterboxed to preserve the Mac's aspect ratio, and the pointer coordinates are
mapped accordingly.

## Enabling real decoding (FFmpeg)

By default the client builds without FFmpeg: packets are counted but not
decoded. Use `--synthetic-video` to see the renderer, or build with FFmpeg:

```sh
cargo build -p macss-client --features macss-decoder/ffmpeg
```

This needs FFmpeg development libraries at build time:

- **Windows:** install via [vcpkg](https://vcpkg.io) — `vcpkg install ffmpeg:x64-windows`
  and set `VCPKG_ROOT`. `ffmpeg-next`'s `build` feature picks it up.
- **macOS:** `brew install ffmpeg pkg-config`.

The backend lives behind `macss_decoder::VideoDecoder`; a native
Media Foundation / VideoToolbox implementation can replace it later without
touching the transport or the renderer.

## Verified in this environment

- `cargo test --workspace`: 39 tests — protocol framing and reassembly, control
  round-trips, UDP send/receive, backoff, arg parsing, HID key mapping, scroll
  direction, and letterbox coordinate math.
- `tests/end_to_end.rs`: a mock host streams frames and the client completes on
  `bye`; a second test injects a `MouseMove` and asserts the host receives it.
- Windowed client launches a wgpu (Vulkan) surface, receives synthetic frames
  and renders them without validation errors.
