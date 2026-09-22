#!/bin/bash
# Sign the CI-built Mac app/CLI and create notarized release archives and a DMG.
set -euo pipefail
if [[ $# != 3 ]]; then
  echo "Usage: ZFSE_SIGN_IDENTITY=... ZFSE_NOTARY_PROFILE=... $0 INPUT_DIR OUTPUT_DIR arm64|x86_64" >&2
  exit 2
fi
: "${ZFSE_SIGN_IDENTITY:?Set a Developer ID Application signing identity}"
: "${ZFSE_NOTARY_PROFILE:?Set an existing notarytool Keychain profile}"
INPUT=$(cd "$1" && pwd)
OUTPUT="$2"
ARCH="$3"
case "$ARCH" in arm64|x86_64) ;; *) echo "Unsupported Mac architecture: $ARCH" >&2; exit 2 ;; esac
APP_ZIP="zfs-send-explore-macos-$ARCH.zip"
CLI_TAR="zfs-send-extract-macos-$ARCH.tar.gz"
test -f "$INPUT/$APP_ZIP"
test -f "$INPUT/$CLI_TAR"
if [[ -e "$OUTPUT" ]]; then
  echo "Output directory already exists; use a fresh directory: $OUTPUT" >&2
  exit 2
fi
mkdir -p "$OUTPUT"
OUTPUT=$(cd "$OUTPUT" && pwd)
STAGE="$OUTPUT/work/payload"
EVIDENCE="$OUTPUT/notarization-evidence"
mkdir -p "$STAGE" "$EVIDENCE" "$OUTPUT/work/cli"
/usr/bin/ditto -x -k "$INPUT/$APP_ZIP" "$STAGE"
tar -xzf "$INPUT/$CLI_TAR" -C "$OUTPUT/work/cli"
cp "$OUTPUT/work/cli/zfs-send-extract" "$STAGE/"
cp "$OUTPUT/work/cli/LICENSE" "$STAGE/"
APP="$STAGE/ZFS Explore.app"
CLI="$STAGE/zfs-send-extract"
SERVICE="$APP/Contents/MacOS/zfs-explore-service"
for binary in "$APP/Contents/MacOS/ZFSExplore" "$SERVICE" "$CLI"; do
  test "$(lipo -archs "$binary")" = "$ARCH"
done
VERSION=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$APP/Contents/Info.plist")
cat > "$STAGE/README.txt" <<README
ZFS Explore $VERSION for macOS 13 or later ($ARCH)

App: drag ZFS Explore.app to Applications, then open it normally.
CLI: copy zfs-send-extract to a writable folder and run it from Terminal.
For example, after copying it into your current directory:
  ./zfs-send-extract --help

The app, bundled service and standalone CLI are Developer ID signed and
Apple-notarized. The app and this disk image carry stapled notarization tickets.
The standalone CLI tarball contains the same signed/notarized executable, but
bare command-line executables cannot carry a stapled ticket. Prefer the disk
image when preparing for use without internet access.

Normal first-open confirmation from macOS may still appear. Signing does not
override an organization's device policy or the supported macOS/CPU requirements.

Documentation: https://github.com/amcchord/zfs-send-explore
README
ln -s /Applications "$STAGE/Applications"

# Sign nested code first. No relaxed hardened-runtime entitlements are required.
codesign --force --sign "$ZFSE_SIGN_IDENTITY" --options runtime --timestamp \
  --identifier net.mcchord.zfs-send-explore.service "$SERVICE"
codesign --force --sign "$ZFSE_SIGN_IDENTITY" --options runtime --timestamp \
  --identifier net.mcchord.zfs-send-extract "$CLI"
codesign --force --sign "$ZFSE_SIGN_IDENTITY" --options runtime --timestamp "$APP"
codesign --verify --deep --strict "$APP"
codesign --verify --strict "$CLI"

notarize() {
  local archive="$1" label="$2" submission_id
  xcrun notarytool submit "$archive" --keychain-profile "$ZFSE_NOTARY_PROFILE" \
    --wait --timeout 20m --output-format json > "$EVIDENCE/$label-submission.json"
  submission_id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' \
    "$EVIDENCE/$label-submission.json")
  xcrun notarytool log "$submission_id" --keychain-profile "$ZFSE_NOTARY_PROFILE" \
    "$EVIDENCE/$label-log.json"
  python3 - "$EVIDENCE/$label-submission.json" "$EVIDENCE/$label-log.json" <<'PY'
import json, sys
for path in sys.argv[1:]:
    report = json.load(open(path))
    if report.get("status") != "Accepted":
        raise SystemExit(f"Notarization was not accepted; inspect {path}")
print("Apple accepted notarization.")
PY
}

# This submission covers the GUI, its worker and the independently distributed CLI.
/usr/bin/ditto -c -k --sequesterRsrc "$STAGE" "$OUTPUT/work/submission.zip"
notarize "$OUTPUT/work/submission.zip" binaries
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
codesign --verify --deep --strict --check-notarization "$APP"
codesign --verify --strict --check-notarization "$SERVICE"
codesign --verify --strict --check-notarization "$CLI"
spctl --assess --type execute --verbose=4 "$APP" 2> "$EVIDENCE/app-gatekeeper.txt"

# Re-archive only after stapling the application ticket.
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP" "$OUTPUT/$APP_ZIP"
COPYFILE_DISABLE=1 tar -czf "$OUTPUT/$CLI_TAR" -C "$STAGE" zfs-send-extract README.txt LICENSE
DMG="$OUTPUT/zfs-send-explore-macos-$ARCH.dmg"
hdiutil create -volname "ZFS Explore $ARCH" -srcfolder "$STAGE" -format UDZO "$DMG"
codesign --force --sign "$ZFSE_SIGN_IDENTITY" --timestamp \
  --identifier "net.mcchord.zfs-send-explore.diskimage.$ARCH" "$DMG"
notarize "$DMG" disk-image
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
spctl --assess --type open --context context:primary-signature --verbose=4 "$DMG" \
  2> "$EVIDENCE/dmg-gatekeeper.txt"
(cd "$OUTPUT" && shasum -a 256 "$APP_ZIP" "$CLI_TAR" "$(basename "$DMG")" > SHA256SUMS.txt)
printf 'Signed and notarized macOS %s release files: %s\n' "$ARCH" "$OUTPUT"
