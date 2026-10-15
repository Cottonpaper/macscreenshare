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
`--no-cursor`, `--verbose`.

## Permissions

Grant these in System Settings → Privacy & Security (the menu bar has shortcuts):

1. **Screen Recording** — required by ScreenCaptureKit.
2. **Accessibility** — required to inject mouse/keyboard.
3. **Local Network** (macOS 15+) — required to accept LAN connections.

Wake-on-LAN additionally needs **Wake for network access** enabled.

The host only *checks* permissions at launch and never pops a prompt on its own:
use the menu-bar items to grant them, or let ScreenCaptureKit ask on the first
capture.

> Launch the bundle with `open dist/MacScreenShare.app` rather than running the
> binary (or `swift run`) from a shell. macOS ties a permission prompt to the
> **responsible** process — a shell-launched binary makes it say *Terminal*
> instead of *MacScreenShare* — and the grant is remembered against that
> identity. Prefer the `.app`.

## Status

Compiles and runs on macOS 15: `swift build` and `./scripts/build-host-app.sh`
both succeed, and the app captures, encodes and streams to the Rust client
(which decodes with VideoToolbox). The first-build signature fixes are done.
