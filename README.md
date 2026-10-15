# ![WARNING! AI SLOP](https://img.shields.io/badge/-WARNING!_AI_SLOP-red?style=for-the-badge)

> [!WARNING]
> This repository was largely AI-generated with minimal human review. Verify before relying on it.

# macscreenshare

Mirror a macOS screen to a Windows or macOS client over Wi-Fi, with mouse and
keyboard passthrough so the client can drive the Mac, plus text clipboard sync
and Wake-on-LAN.

- **Host:** native macOS menu-bar app (Swift, ScreenCaptureKit + VideoToolbox)
- **Client:** cross-platform Rust app (Windows/macOS)
- **Transport:** UDP for video, TCP for control
- **Target:** interactive desktop — crisp text, 30–60 fps, ~50–100 ms on a LAN

See [`docs/protocol.md`](docs/protocol.md) for the normative wire format.

## Status

Early but runnable. The Rust client connects, receives video, renders it in a
window, and forwards mouse/keyboard back to the host. Both platforms decode
natively out of the box: VideoToolbox on macOS, Media Foundation on Windows —
hardware accelerated, with no FFmpeg or extra packages needed. FFmpeg remains an
optional alternative backend, and `--synthetic-video` renders a local test
pattern so you can exercise the renderer without a host. The macOS host builds
against the real Apple frameworks and now compiles and runs on a Mac.

| Area | State |
|---|---|
| Wire protocol + framing + reassembly | done, unit-tested |
| Rust transport, backoff, Wake-on-LAN, args | done, unit-tested |
| Client session (handshake, stream, stats, reconnect, input) | done, end-to-end tested against a mock host |
| Client window + wgpu YUV renderer + input capture | done, launches and renders (Vulkan on Windows, Metal on macOS) |
| VideoToolbox decode backend (macOS) | done, hardware decode, default on macOS |
| Media Foundation decode backend (Windows) | done, native H.264 decode by default (HEVC needs the OS "HEVC Video Extensions") |
| FFmpeg decode backend | optional alternative, behind the `ffmpeg` feature |
| macOS host: capture, encode, input injection, clipboard, menu bar | builds and runs on macOS |
| macOS host: app bundle + Info.plist + build script | provided |

## Building the client

```sh
cd client
cargo test --workspace
cargo run -p macss-mock-host -- --seconds 60     # terminal 1
cargo run -p macss-client -- 127.0.0.1 --synthetic-video   # terminal 2
```

`--synthetic-video` renders a local test pattern so you can see the window and
input path without a host. Real decoding works natively on both platforms —
VideoToolbox on macOS, Media Foundation on Windows; FFmpeg is an optional
alternative. See [`client/README.md`](client/README.md) for details.

## Building the host (on macOS)

```sh
swift build -c release --package-path host-macos
# or produce dist/MacScreenShare.app:
./scripts/build-host-app.sh
```

Run it and grant the permissions from the menu bar (see below) — or let
ScreenCaptureKit ask on the first capture — then point the client at the address
shown in the menu bar:

```sh
cargo run -p macss-client -- 192.168.1.23 --token <secret>
```

## macOS permissions the host needs

- **Screen Recording** — required by ScreenCaptureKit.
- **Accessibility** — required to inject mouse/keyboard events.
- **Local Network** (macOS 15+) — for accepting connections from the LAN.
- **Wake for network access** — must be enabled in System Settings for Wake-on-LAN.

Permissions are tied to the app's code signature, so rebuilding with a different
identity can require re-granting. `scripts/build-host-app.sh` uses an ad-hoc
signature to keep that stable.

## Architecture

```
macOS host (Swift)                          client (Rust)
  ScreenCaptureKit ─► VideoToolbox ─┐        ┌─ UDP video ─► reassembly ─► decoder ─► wgpu render
                                    ├── Wi-Fi ┤
  CGEvent inject ◄── input ◄────────┘        └─ TCP control ─► session, input, clipboard
  NSPasteboard  ◄──► clipboard                  Wake-on-LAN before connecting
```

- **Video** is sent as datagrams with a 16-byte header (frame id, chunk index,
  flags, codec, timestamp). Idempotent, order-tolerant; recovery is by
  requesting a fresh keyframe rather than retransmission.
- **Control** is length-prefixed JSON over TCP: handshake, input events,
  clipboard, heartbeats, stats. Coordinates are normalized `[0,1]` and key
  events use USB HID usage ids so Windows and macOS agree on keys.

## Roadmap

1. **M0/M1:** protocol, transport, host capture/encode, client receive + stats. ✅
2. **M2:** client window + GPU render (winit + wgpu), input capture → host injection,
   and pointer capture (click to lock, Esc to release). ✅
3. **M3:** native VideoToolbox decode on macOS
   ([`docs/videotoolbox-decode.md`](docs/videotoolbox-decode.md)), so no FFmpeg
   is needed on a Mac — plus robustness: keyframe recovery, adaptive bitrate,
   sleep prevention and the permission UX (stable code-signing identity,
   non-repeating prompts). ✅
4. **M4:** clipboard polish (the wire path exists), multi-display selection.
5. **M5:** packaging (Windows installer, notarized macOS `.app`), end-to-end docs.

## Not in scope yet

Audio, file transfer, internet/NAT traversal (LAN only for now), end-to-end
encryption (a shared `--token` is the only auth in v1).
