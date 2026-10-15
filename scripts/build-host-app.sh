#!/usr/bin/env bash
# Build the macOS host and wrap it in a .app bundle.
#
# Run from anywhere: scripts/build-host-app.sh
# Output: dist/MacScreenShare.app  (ad-hoc signed)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> swift build (release)"
swift build -c release --package-path host-macos

APP="dist/MacScreenShare.app"
echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp host-macos/.build/release/macss-host "$APP/Contents/MacOS/macss-host"
cp host-macos/Info.plist "$APP/Contents/Info.plist"

# Ad-hoc signature keeps TCC permissions stable across rebuilds. Replace the
# identity with a Developer ID for distribution.
echo "==> codesign (ad-hoc)"
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 \
    || echo "   (codesign skipped; permissions may need re-granting after each build)"

echo "==> done: $APP"
echo "    open \"$APP\""
