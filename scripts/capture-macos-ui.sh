#!/bin/bash
# Capture the Mac app's main screens from the public test fixtures.
# Usage: scripts/capture-macos-ui.sh [OUTPUT_DIR]
#   ZFSE_CAPTURE_DARK=1                 capture in Dark Mode
#   ZFSE_CAPTURE_WIDTH / _HEIGHT        window size (default 1180x760)
#   ZFSE_CAPTURE_SLIDE / _SLIDE_KEY     optional real Slide pool image and key file
#   ZFSE_CAPTURE_DRIVE=1                also open the first attached drive (needs _SLIDE_KEY)
set -euo pipefail
cd "$(dirname "$0")/.."
OUTPUT=$(mkdir -p "${1:-target/macos-ui}" && cd "${1:-target/macos-ui}" && pwd)
ARCH=$(uname -m)
case "$ARCH" in arm64) RUST_TARGET=aarch64-apple-darwin ;; *) RUST_TARGET=x86_64-apple-darwin ;; esac
WORK="$PWD/target/macos-ui-capture"
APP="$WORK/Capture.app"
rm -rf "$WORK" && mkdir -p "$APP/Contents/MacOS" "$WORK/sources" "$WORK/inputs"
cargo build --release --locked --target "$RUST_TARGET" --bin zfs-explore-service
cp "target/$RUST_TARGET/release/zfs-explore-service" "$APP/Contents/MacOS/"
cp packaging/icons/ZFSExplore.icns "$WORK/"
mkdir -p "$APP/Contents/Resources" && cp packaging/icons/ZFSExplore.icns "$APP/Contents/Resources/"
printf '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>net.mcchord.zfs-send-explore.capture</string><key>CFBundleExecutable</key><string>Capture</string><key>CFBundleIconFile</key><string>ZFSExplore.icns</string></dict></plist>' > "$APP/Contents/Info.plist"
# The tour supplies its own entry point instead of the app's @main scene.
for source in macos/*.swift; do sed 's/^@main struct ZFSExplore/struct ZFSExplore/' "$source" > "$WORK/sources/$(basename "$source")"; done
swiftc -parse-as-library -O -target "$ARCH-apple-macosx14.0" -o "$APP/Contents/MacOS/Capture" "$WORK"/sources/*.swift scripts/macos-ui-capture.swift
codesign --force --sign - "$APP/Contents/MacOS/zfs-explore-service" "$APP"
base64 -d < tests/fixtures/inception/ext4.img.zst.b64 | zstd -d -q -o "$WORK/inputs/ext4.img"
printf 'This is a note, not a backup.\n' > "$WORK/inputs/notes.txt"
ZFSE_CAPTURE_FIXTURES="$PWD/tests/fixtures" ZFSE_CAPTURE_INPUTS="$WORK/inputs" "$APP/Contents/MacOS/Capture" "$OUTPUT"
echo "Screenshots: $OUTPUT"
