#!/bin/bash
# Exercise the actual archived app through its open, unlock and save windows.
set -euo pipefail
if [ "$#" -ne 2 ]; then
  echo "Usage: $0 <packaged-app.zip> <new-evidence-directory>" >&2
  exit 2
fi
repo=$(cd "$(dirname "$0")/.." && pwd)
archive=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
mkdir "$2"
output=$(cd "$2" && pwd)
mkdir -p "$output"/{app,restored,occupied}
shasum -a 256 "$archive" > "$output/archive.sha256"
ditto -x -k "$archive" "$output/app"
app="$output/app/ZFS Explore.app"
codesign --verify --deep --strict "$app"
shasum -a 256 "$app/Contents/MacOS/"{ZFSExplore,zfs-explore-service} > "$output/binaries.sha256"
xcodebuild -version > "$output/xcode-version.txt"
ZFSE_GUI_APP="$app" ZFSE_GUI_FIXTURES="$repo/tests/fixtures" ZFSE_GUI_EVIDENCE="$output" \
xcodebuild test -project "$repo/tests/macos-ui/PackagedApp.xcodeproj" -scheme PackagedAppTests \
  -destination "platform=macOS,arch=$(uname -m)" -derivedDataPath "$output/derived" \
  -parallel-testing-enabled NO -test-timeouts-enabled YES -default-test-execution-time-allowance 180 \
  -maximum-test-execution-time-allowance 240 -resultBundlePath "$output/Qualification.xcresult" \
  "ZFSE_GUI_APP=$app" "ZFSE_GUI_FIXTURES=$repo/tests/fixtures" "ZFSE_GUI_EVIDENCE=$output" \
  | tee "$output/xcodebuild.log"
