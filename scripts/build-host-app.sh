#!/usr/bin/env bash
# Build the macOS host and wrap it in a .app bundle.
#
# Run from anywhere: scripts/build-host-app.sh
# Output: dist/MacScreenShare.app  (signed with a stable identity if available)
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

# TCC (Screen Recording / Accessibility / Local Network) remembers a grant
# against the app's code signature. An ad-hoc signature ("-") changes with
# every build, so grants silently stop working after each rebuild even though
# System Settings still shows them enabled. Sign with a stable identity:
#
# Create a stable one once with:
#
#   ./scripts/make-signing-cert.sh
#
# or pass an existing identity: MACSS_SIGN_IDENTITY="Apple Development: …" scripts/build-host-app.sh
# Without any identity, ad-hoc signing is used.
IDENTITY="${MACSS_SIGN_IDENTITY:-}"
if [[ -z "$IDENTITY" ]]; then
    # Prefer the certificate from scripts/make-signing-cert.sh, else any identity.
    PREFERRED="${MACSS_CERT_NAME:-MacScreenShare Dev Signing}"
    IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
        | awk -F'"' -v want="$PREFERRED" '$2 == want { print $2; exit }')"
fi
if [[ -z "$IDENTITY" ]]; then
    IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
        | awk -F'"' '/"/ { print $2; exit }')"
fi

if [[ -n "$IDENTITY" ]]; then
    echo "==> codesign ($IDENTITY)"
    codesign --force --options runtime --sign "$IDENTITY" "$APP"
else
    echo "==> codesign (ad-hoc)"
    echo "   WARNING: no signing identity found. Permissions will need to be"
    echo "   re-granted after every rebuild. Create one once with:"
    echo "     ./scripts/make-signing-cert.sh"
    echo "   If they appear enabled but don't work, run:"
    echo "     tccutil reset ScreenCapture dev.macss.host"
    echo "     tccutil reset Accessibility dev.macss.host"
    codesign --force --sign - "$APP"
fi
codesign --verify --verbose=1 "$APP"

echo "==> done: $APP"
echo "    open \"$APP\""
