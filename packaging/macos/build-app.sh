#!/usr/bin/env bash
# Build a macOS .app bundle for kame.
#
# Run on a Mac with the Rust toolchain and Xcode Command Line Tools installed
# (CLT provides the C compiler for `rusqlite`/bundled and the `codesign` and
# `iconutil` tools). Produces `target/kame.app`, ad-hoc signed for local use.
#
# For a universal (Intel + Apple Silicon) bundle, build both targets and merge
# the binaries with `lipo -create`, then point this script at the merged binary.
set -euo pipefail

cd "$(dirname "$0")/../.."   # repository root

APP_NAME="kame"
APP="target/${APP_NAME}.app"

# Release build for the host architecture.
cargo build --release

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/release/${APP_NAME}" "$APP/Contents/MacOS/${APP_NAME}"

# Icon: `iconutil` consumes an `.iconset` of 8-bit PNGs at the canonical sizes.
ICONSET="target/${APP_NAME}.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" packaging/kame.png \
        --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" packaging/kame.png \
        --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/${APP_NAME}.icns"
rm -rf "$ICONSET"

cp packaging/macos/Info.plist "$APP/Contents/Info.plist"

# Ad-hoc sign for local use. Distributing to others needs a Developer ID
# certificate and `notarytool` notarization instead.
codesign --force --sign - "$APP"

echo "built ${APP}"
