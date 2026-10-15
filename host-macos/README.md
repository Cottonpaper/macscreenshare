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

> If you run via `swift run` instead of the bundle, macOS attributes the
> permissions to **Terminal**, not to this binary. Prefer the `.app`.

## Status

Authored against the real frameworks but **not yet compiled** — it was written
on a machine without a Swift toolchain. Expect a handful of small signature
fixes on the first `swift build`. The most likely candidates are noted in the
commit history; the `wgpu`/`winit` Rust client side is already verified.
