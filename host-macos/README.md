# macss-host (macOS)

Native menu-bar host: capture the screen with ScreenCaptureKit, encode with
VideoToolbox, send video over UDP, inject mouse/keyboard via CGEvent, and sync
the clipboard. Speaks the protocol in [`../docs/protocol.md`](../docs/protocol.md).

## Requirements

- macOS 13 or newer
- Xcode 15+ / Swift 5.9+ (`swift --version`)

## Build and run

```sh
# from the repository root
swift build --package-path host-macos
swift run --package-path host-macos macss-host -- --token mysecret
```

Or build a proper `.app` bundle (recommended — TCC permissions attach to the
app, not to Terminal):

```sh
./scripts/build-host-app.sh
open dist/MacScreenShare.app
```

Options: `--token <T>`, `--port <P>`, `--fps <N>`, `--bitrate <K>`,
`--no-cursor`, `--idle-refresh <N>`, `--verbose`.

`--idle-refresh` sets how often a *static* screen is still refreshed (default
10 fps). ScreenCaptureKit marks frames `idle` when nothing changed; the host
forwards those at this rate so the client keeps converging on the current screen
(cursor, caret, and updates ScreenCaptureKit doesn't flag) instead of freezing on
the last frame. `--idle-refresh 0` restores the old "only send on change"
behaviour.

## Permissions

Grant these in System Settings → Privacy & Security (the menu bar has shortcuts):

1. **Screen Recording** — required by ScreenCaptureKit.
2. **Accessibility** — required to inject mouse/keyboard.
3. **Local Network** (macOS 15+) — required to accept LAN connections.

Wake-on-LAN additionally needs **Wake for network access** enabled.

The host only *checks* permissions at launch and never pops a prompt on its own:
use the menu-bar items to grant them, or let ScreenCaptureKit ask on the first
capture.

> **Signing matters.** macOS ties each permission grant to the app's code
> signature. An ad-hoc signature changes on every build, so a grant can still
> show as enabled in System Settings but not actually work. Create a stable
> identity once (no sudo, no Keychain GUI):
>
> ```sh
> ./scripts/make-signing-cert.sh   # one-time; safe to re-run
> ./scripts/build-host-app.sh      # signs with it automatically
> ```
>
> To clear stale grants:
> `tccutil reset ScreenCapture dev.macss.host && tccutil reset Accessibility dev.macss.host`.
>
> A new Screen Recording grant only takes effect after the host restarts; use
> the menu's **Relaunch to apply permissions** item.

> Launch the bundle with `open dist/MacScreenShare.app` rather than running the
> binary (or `swift run`) from a shell. macOS ties a permission prompt to the
> **responsible** process — a shell-launched binary makes it say *Terminal*
> instead of *MacScreenShare* — and the grant is remembered against that
> identity. Prefer the `.app`.

## While streaming

The host holds a `ProcessInfo` activity assertion for the life of a stream, so
the Mac and its display stay awake while a client is connected. The assertion is
released as soon as the stream stops (client disconnect, `stop_stream`, `bye`,
or control timeout) — including across reconnects, so it never leaks. This is
what stops an idle Mac from sleeping mid-session and dropping the client.

The host advertises the `adaptive_bitrate` capability in `hello_ack`. A client
that supports it can change the encoder's target bitrate mid-stream with
`set_bitrate` (clamped to 500–200000 kbit/s); the change takes effect over the
next few frames and does not force a keyframe.

## Status

Compiles and runs on macOS 15: `swift build` and `./scripts/build-host-app.sh`
both succeed, and the app captures, encodes and streams to the Rust client
(which decodes with VideoToolbox). The first-build signature fixes are done.
