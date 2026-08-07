#!/bin/sh
set -eu

binary=${1:-target/release/zfs-send-extract}
member=${2:?usage: verify-pool-member.sh BINARY MEMBER DATASET@SNAPSHOT PATH EXPECTED_SHA256 [OUTPUT_DIR]}
snapshot=${3:?usage: verify-pool-member.sh BINARY MEMBER DATASET@SNAPSHOT PATH EXPECTED_SHA256 [OUTPUT_DIR]}
path=${4:?usage: verify-pool-member.sh BINARY MEMBER DATASET@SNAPSHOT PATH EXPECTED_SHA256 [OUTPUT_DIR]}
expected_sha256=${5:?usage: verify-pool-member.sh BINARY MEMBER DATASET@SNAPSHOT PATH EXPECTED_SHA256 [OUTPUT_DIR]}
output_dir=${6:-.artifacts/pool-member}

case "$snapshot" in
    *@*) ;;
    *)
        echo 'verification requires a named dataset@snapshot selector' >&2
        exit 1
        ;;
esac

mkdir -p "$output_dir"
target="$output_dir/extracted.bin"

"$binary" pool inspect "$member" --json > "$output_dir/inspection.json"
"$binary" pool datasets "$member" > "$output_dir/datasets.txt"
"$binary" pool snapshots "$member" "${snapshot%@*}" > "$output_dir/snapshots.txt"
"$binary" pool extract "$member" "$snapshot" "$path" --output "$target" --force

actual_sha256=$(sha256sum "$target" | awk '{print $1}')
if [ "$actual_sha256" != "$expected_sha256" ]; then
    echo "pool-member extraction hash mismatch: expected $expected_sha256, got $actual_sha256" >&2
    exit 1
fi
if [ ! -f "$target.zfse.json" ]; then
    echo 'named-snapshot extraction did not write its incremental-send sidecar' >&2
    exit 1
fi

printf 'verified pool-member extraction: size=%s sha256=%s\n' \
    "$(stat -c '%s' "$target")" "$actual_sha256"
