#!/bin/bash

# Build a plugin/update ZIP the way .github/workflows/build-plugins.yml does,
# from a binary already built for the device (see update_service.sh for the
# container build). The gateway itself is packaged this way: the ZIP is what
# the installer's "upload" takes, locally or pushed over the radio from the
# Remote page.
#
# Usage: ./scripts/package_plugin.sh <plugin_id> [--root <repo>] [--sign <key.pem>] [--out <dir>]
#
#   <plugin_id>   crate directory, binary and service stem (kaonic-gateway,
#                 kaonic-plugin-sample, kaonic-commd, ...): expects, under
#                 the repo root,
#                   <plugin_id>/kaonic-plugin.toml
#                   <plugin_id>/<plugin_id>.service
#                   <plugin_id>/files/            (optional, shipped as files/)
#                   target/<triple>/release/<plugin_id>
#   --root        repo root (default: this repo; ../kaonic-radio for commd)
#   --sign        OTA signing key; without it the package installs as unofficial.
#   --out         output directory (default deploy/<plugin_id> in this repo)
#
# ZIP layout (entries at the root, no top-level folder):
#   kaonic-plugin.toml  <plugin_id>.service  <plugin_id>  <plugin_id>.sha256
#   [<plugin_id>.sig]   [files/...]

set -euo pipefail

if [ "$#" -lt 1 ]; then
    sed -n '3,24p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
fi

PLUGIN_ID="$1"
shift
SIGN_KEY=""
OUT_DIR=""
SRC_ROOT=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --root) SRC_ROOT="$2"; shift 2 ;;
        --sign) SIGN_KEY="$2"; shift 2 ;;
        --out) OUT_DIR="$2"; shift 2 ;;
        *) echo "unknown option: $1" >&2; exit 1 ;;
    esac
done

TARGET_TRIPLE="${TARGET_TRIPLE:-armv7-unknown-linux-gnueabihf}"
BUILD_PROFILE="${BUILD_PROFILE:-release}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "${SRC_ROOT:-$HERE}" && pwd)"
OUT_DIR="${OUT_DIR:-$HERE/deploy/$PLUGIN_ID}"

MANIFEST="$ROOT/$PLUGIN_ID/kaonic-plugin.toml"
SERVICE="$ROOT/$PLUGIN_ID/$PLUGIN_ID.service"
FILES_DIR="$ROOT/$PLUGIN_ID/files"
BINARY="$ROOT/target/$TARGET_TRIPLE/$BUILD_PROFILE/$PLUGIN_ID"

for f in "$MANIFEST" "$SERVICE" "$BINARY"; do
    if [ ! -f "$f" ]; then
        echo "missing: $f" >&2
        exit 1
    fi
done

# The manifest version is what the installer records and the UI shows; keep
# it in step with the crate so an update is recognisable as one.
MANIFEST_VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$MANIFEST" | head -1)
CRATE_VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/$PLUGIN_ID/Cargo.toml" | head -1)
if [ "$MANIFEST_VERSION" != "$CRATE_VERSION" ]; then
    echo "warning: manifest version $MANIFEST_VERSION != crate version $CRATE_VERSION" >&2
fi

if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | awk '{print $1}'; }
else
    sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
fi

STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT

cp "$MANIFEST" "$STAGING/kaonic-plugin.toml"
cp "$SERVICE" "$STAGING/$PLUGIN_ID.service"
cp "$BINARY" "$STAGING/$PLUGIN_ID"
chmod 755 "$STAGING/$PLUGIN_ID"
sha256 "$STAGING/$PLUGIN_ID" > "$STAGING/$PLUGIN_ID.sha256"

if [ -n "$SIGN_KEY" ]; then
    openssl dgst -sha256 -sign "$SIGN_KEY" -out "$STAGING/$PLUGIN_ID.sig" "$STAGING/$PLUGIN_ID"
    PUB="$(mktemp)"
    openssl pkey -in "$SIGN_KEY" -pubout -out "$PUB" >/dev/null 2>&1
    openssl dgst -sha256 -verify "$PUB" -signature "$STAGING/$PLUGIN_ID.sig" "$STAGING/$PLUGIN_ID" >/dev/null
    rm -f "$PUB"
    SIGNED="signed"
else
    SIGNED="unsigned"
fi

if [ -d "$FILES_DIR" ]; then
    cp -R "$FILES_DIR" "$STAGING/files"
fi

mkdir -p "$OUT_DIR"
ZIP="$OUT_DIR/$PLUGIN_ID-$MANIFEST_VERSION.zip"
rm -f "$ZIP"
(
    cd "$STAGING"
    zip -q -r -X "$ZIP" .
)

echo "$ZIP ($SIGNED, binary built $(date -r "$BINARY" '+%Y-%m-%d %H:%M'))"
unzip -l "$ZIP" | awk 'NR > 3 && NF >= 4 && $1 ~ /^[0-9]+$/ { printf "  %10s  %s\n", $1, $4 }'
