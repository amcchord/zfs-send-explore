#!/bin/sh
set -eu

dataset=${1:-labpool/zfs-send-extract}
output_dir=${2:-/root/zfs-send-fixtures}
mountpoint=$(zfs get -H -o value mountpoint "$dataset" 2>/dev/null || true)

if [ -n "$mountpoint" ] && [ "$mountpoint" != "-" ]; then
    echo "dataset $dataset already exists; refusing to overwrite it" >&2
    exit 1
fi

mkdir -p "$output_dir"
zfs create -o compression=off -o recordsize=128K "$dataset"
mountpoint=$(zfs get -H -o value mountpoint "$dataset")
mkdir -p "$mountpoint/payload" "$mountpoint/docs" "$mountpoint/nested/config"

printf '%s\n' 'ZFS send extraction fixture' > "$mountpoint/docs/readme.txt"
printf '%s\n' '{"fixture":true,"version":1}' > "$mountpoint/nested/config/settings.json"
printf '%s\n' 'a second ordinary file proves the dataset is not a one-file volume' > "$mountpoint/notes.txt"

python3 - "$mountpoint/payload/large.bin" <<'PY'
import hashlib
import sys

path = sys.argv[1]
size = 20 * 1024 * 1024
with open(path, "wb") as output:
    counter = 0
    while output.tell() < size:
        block = hashlib.sha256(f"zfs-send-base:{counter}".encode()).digest() * 4096
        output.write(block[: min(len(block), size - output.tell())])
        counter += 1
PY

zfs snapshot "$dataset@base"
zfs send "$dataset@base" > "$output_dir/full.zfs"
sha256sum "$mountpoint/payload/large.bin" | awk '{print $1}' > "$output_dir/base.sha256"
stat -c '%s' "$mountpoint/payload/large.bin" > "$output_dir/base.size"

python3 - "$mountpoint/payload/large.bin" <<'PY'
import hashlib
import os
import sys

path = sys.argv[1]
with open(path, "r+b") as output:
    for offset, label in ((64 * 1024, b"first-change"), (9 * 1024 * 1024, b"middle-change"), (19 * 1024 * 1024, b"tail-change")):
        output.seek(offset)
        output.write(hashlib.sha256(label).digest() * 2048)
    output.seek(0, os.SEEK_END)
    for counter in range(16):
        output.write(hashlib.sha256(f"zfs-send-increment:{counter}".encode()).digest() * 4096)
PY
printf '%s\n' '{"fixture":true,"version":2}' > "$mountpoint/nested/config/settings.json"
printf '%s\n' 'created after the base snapshot' > "$mountpoint/new-in-increment.txt"

zfs snapshot "$dataset@incremental"
zfs send -i "$dataset@base" "$dataset@incremental" > "$output_dir/incremental.zfs"
sha256sum "$mountpoint/payload/large.bin" | awk '{print $1}' > "$output_dir/incremental.sha256"
stat -c '%s' "$mountpoint/payload/large.bin" > "$output_dir/incremental.size"

zfs list -t snapshot -o name,guid,refer,creation "$dataset@base" "$dataset@incremental" > "$output_dir/snapshots.txt"
ls -lh "$output_dir/full.zfs" "$output_dir/incremental.zfs"
printf 'base:        size=%s sha256=%s\n' "$(cat "$output_dir/base.size")" "$(cat "$output_dir/base.sha256")"
printf 'incremental: size=%s sha256=%s\n' "$(cat "$output_dir/incremental.size")" "$(cat "$output_dir/incremental.sha256")"

