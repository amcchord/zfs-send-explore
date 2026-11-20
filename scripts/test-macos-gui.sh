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
echo 'keep this existing file' > "$output/occupied/hello.txt"
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

# The sandboxed UI runner attaches screenshots to its XCTest result. Validate the
# actual files from the calling process, which owns the disposable destinations.
(
  cd "$output"
  shasum -a 256 -c <<'HASHES' | tee verification.txt
6e219bc096755477b1a30301446ac364f6291f16ce381d561947cdecd3fab265  restored/hello.txt
4a5b121833313a53cc3120afd66cb662c2e7b23f839ef547e9e58576c52e48e0  restored/version.txt
880b6e73a4f158b5df9ccc251d518fa5f385008ac5f26153e9b8451077fdcbc0  occupied/hello.txt
HASHES
  shasum -a 256 restored/hello.txt restored/version.txt occupied/hello.txt >> verification.txt
)
