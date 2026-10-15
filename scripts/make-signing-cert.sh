#!/usr/bin/env bash
# Create a stable, self-signed code-signing certificate for the macOS host.
#
# Why this exists
# ---------------
# macOS TCC (Screen Recording, Accessibility, Local Network) keys each grant to
# the app's code signature. An ad-hoc signature ("-") changes on every build, so
# grants stop matching after a rebuild even though System Settings still shows
# them enabled. Signing with a certificate makes the signature stable, so grants
# survive rebuilds.
#
# This is a one-time setup and is safe to re-run: if the identity already
# exists, it does nothing. Nothing is installed system-wide and no sudo is needed.
#
# Usage:
#   scripts/make-signing-cert.sh
#
# Then build as usual; scripts/build-host-app.sh picks the identity up
# automatically (or set MACSS_SIGN_IDENTITY to choose explicitly).

set -euo pipefail

CERT_NAME="${MACSS_CERT_NAME:-MacScreenShare Dev Signing}"
CERT_DIR="${MACSS_CERT_DIR:-$HOME/Library/Application Support/macss/signing}"
KEYCHAIN="${MACSS_KEYCHAIN:-$HOME/Library/Keychains/login.keychain-db}"
DAYS="${MACSS_CERT_DAYS:-3650}"

if [[ ! -f "$KEYCHAIN" ]]; then
    echo "error: login keychain not found at $KEYCHAIN" >&2
    echo "       set MACSS_KEYCHAIN to override" >&2
    exit 1
fi

OPENSSL="$(command -v openssl || true)"
if [[ -z "$OPENSSL" && -x /usr/bin/openssl ]]; then
    OPENSSL=/usr/bin/openssl
fi
if [[ -z "$OPENSSL" ]]; then
    echo "error: openssl not found" >&2
    exit 1
fi

# Already present? Nothing to do.
if security find-identity -v -p codesigning "$KEYCHAIN" 2>/dev/null | grep -qF "\"$CERT_NAME\""; then
    echo "==> signing identity \"$CERT_NAME\" already exists; nothing to do"
    security find-identity -v -p codesigning "$KEYCHAIN" | grep -F "\"$CERT_NAME\""
    exit 0
fi

echo "==> generating self-signed code-signing certificate"
echo "    name:   $CERT_NAME"
echo "    stored: $CERT_DIR"
mkdir -p "$CERT_DIR"
chmod 700 "$CERT_DIR"

CONF="$CERT_DIR/openssl.cnf"
KEY="$CERT_DIR/key.pem"
CERT="$CERT_DIR/cert.pem"

# A config file is used instead of -addext because macOS's stock LibreSSL
# openssl does not support -addext.
cat > "$CONF" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no

[dn]
CN = $CERT_NAME

[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

rm -f "$KEY" "$CERT"
"$OPENSSL" req -x509 -newkey rsa:2048 -nodes -sha256 -days "$DAYS" \
    -keyout "$KEY" -out "$CERT" -config "$CONF" 2>/dev/null
chmod 600 "$KEY"

echo "==> importing certificate and key into login keychain"
# PEM is used rather than PKCS#12: OpenSSL 3/4's default p12 encryption is not
# readable by macOS `security import` ("MAC verification failed").
# -T grants codesign access to the private key without a per-build prompt.
security import "$CERT" -k "$KEYCHAIN" \
    -T /usr/bin/codesign -T /usr/bin/security >/dev/null
security import "$KEY" -k "$KEYCHAIN" \
    -T /usr/bin/codesign -T /usr/bin/security >/dev/null

echo "==> trusting certificate for code signing (user trust settings)"
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$CERT"

if security find-identity -v -p codesigning "$KEYCHAIN" 2>/dev/null | grep -qF "\"$CERT_NAME\""; then
    echo
    echo "==> done. Signing identity is ready:"
    security find-identity -v -p codesigning "$KEYCHAIN" | grep -F "\"$CERT_NAME\""
    echo
    echo "    Build the app with a stable signature:"
    echo "      ./scripts/build-host-app.sh"
else
    echo
    echo "error: the certificate was imported but codesign does not see it yet." >&2
    echo "Trust it manually once, then re-run this script:" >&2
    echo "  1. Open Keychain Access," >&2
    echo "  2. find \"$CERT_NAME\", open it, expand Trust," >&2
    echo "  3. set 'Code Signing' to 'Always Trust'." >&2
    echo "Or grant trust system-wide (needs sudo):" >&2
    echo "  sudo security add-trusted-cert -d -r trustRoot -p codeSign -k /Library/Keychains/System.keychain \"$CERT\"" >&2
    exit 1
fi
