#!/bin/bash
# Build a native app and CLI for the host architecture (or ZFSE_MACOS_ARCH).
set -euo pipefail
cd "$(dirname "$0")/.."
ARCH="${ZFSE_MACOS_ARCH:-$(uname -m)}"
case "$ARCH" in
  arm64) RUST_TARGET=aarch64-apple-darwin ;;
  x86_64) RUST_TARGET=x86_64-apple-darwin ;;
  *) echo "Unsupported Mac architecture: $ARCH" >&2; exit 1 ;;
esac
VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
DEST="${ZFSE_PACKAGE_DIR:-$PWD/target/macos-package}"
APP="$DEST/ZFS Explore.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cargo build --release --locked --target "$RUST_TARGET" --bin zfs-send-extract --bin zfs-explore-service
swiftc -parse-as-library -O -target "$ARCH-apple-macosx13.0" \
  -o "$APP/Contents/MacOS/ZFSExplore" macos/ZFSExplore.swift macos/SnapshotTime.swift
cp "target/$RUST_TARGET/release/zfs-explore-service" "$APP/Contents/MacOS/"
cp "target/$RUST_TARGET/release/zfs-send-extract" "$DEST/zfs-send-extract"
"$DEST/zfs-send-extract" --version
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>net.mcchord.zfs-send-explore</string>
<key>CFBundleName</key><string>ZFS Explore</string>
<key>CFBundleDisplayName</key><string>ZFS Explore</string>
<key>CFBundleExecutable</key><string>ZFSExplore</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$VERSION</string>
<key>CFBundleVersion</key><string>$VERSION</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
<key>CFBundleDocumentTypes</key><array><dict>
<key>CFBundleTypeName</key><string>Backup or disk image</string>
<key>CFBundleTypeRole</key><string>Viewer</string>
<key>LSHandlerRank</key><string>Alternate</string>
<key>CFBundleTypeExtensions</key><array><string>zfs</string><string>img</string><string>raw</string><string>qcow2</string><string>vmdk</string></array>
</dict></array>
</dict></plist>
PLIST
# Ad-hoc signing permits local execution; public distribution requires Developer ID + notarization.
codesign --force --sign - "$APP/Contents/MacOS/zfs-explore-service"
codesign --force --sign - "$APP"
codesign --verify --deep --strict "$APP"
cp README.md LICENSE "$DEST/"
cp docs/macos-client.md "$DEST/"
COPYFILE_DISABLE=1 tar -czf "$DEST/zfs-send-extract-macos-$ARCH.tar.gz" -C "$DEST" zfs-send-extract README.md LICENSE
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP" "$DEST/zfs-send-explore-macos-$ARCH.zip"
(cd "$DEST" && shasum -a 256 ./*.zip ./*.tar.gz > SHA256SUMS.txt)
printf 'Built %s\n' "$APP"
