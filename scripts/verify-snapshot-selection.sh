#!/bin/sh
set -eu

binary=${1:-target/release/zfs-send-extract}
dataset=${2:-labpool/zfs-send-snapshots}
output_dir=${3:-.artifacts/snapshot-selection}

if zfs list -H -o name "$dataset" >/dev/null 2>&1; then
    echo "dataset $dataset already exists; refusing to overwrite it" >&2
    exit 1
fi

mkdir -p "$output_dir"
zfs create -o compression=off -o recordsize=128K "$dataset"
mountpoint=$(zfs get -H -o value mountpoint "$dataset")
mkdir -p "$mountpoint/payload" "$mountpoint/siblings"
printf '%s\n' 'unchanged sibling' > "$mountpoint/siblings/unchanged.txt"

python3 - "$mountpoint/payload/large.bin" <<'PY'
import hashlib
import sys

path = sys.argv[1]
size = 20 * 1024 * 1024
with open(path, "wb") as output:
    counter = 0
    while output.tell() < size:
        block = hashlib.sha256(f"snapshot-one:{counter}".encode()).digest() * 4096
        output.write(block[: min(len(block), size - output.tell())])
        counter += 1
PY
zfs snapshot "$dataset@s1"
sha256sum "$mountpoint/payload/large.bin" | awk '{print $1}' > "$output_dir/s1.sha256"
stat -c '%s' "$mountpoint/payload/large.bin" > "$output_dir/s1.size"

python3 - "$mountpoint/payload/large.bin" <<'PY'
import hashlib
import os
import sys

path = sys.argv[1]
with open(path, "r+b") as output:
    for offset, label in ((64 * 1024, b"s2-start"), (10 * 1024 * 1024, b"s2-middle")):
        output.seek(offset)
        output.write(hashlib.sha256(label).digest() * 2048)
    output.seek(0, os.SEEK_END)
    output.write(hashlib.sha256(b"s2-append").digest() * 32768)
PY
printf '%s\n' 'present only in snapshot two' > "$mountpoint/siblings/only-s2.txt"
zfs snapshot "$dataset@s2"
sha256sum "$mountpoint/payload/large.bin" | awk '{print $1}' > "$output_dir/s2.sha256"
stat -c '%s' "$mountpoint/payload/large.bin" > "$output_dir/s2.size"

python3 - "$mountpoint/payload/large.bin" <<'PY'
import hashlib
import sys

path = sys.argv[1]
with open(path, "r+b") as output:
    output.seek(0)
    output.write(hashlib.sha256(b"snapshot-three").digest() * 4096)
    output.truncate(19 * 1024 * 1024)
PY
rm "$mountpoint/siblings/only-s2.txt"
printf '%s\n' 'present only in snapshot three' > "$mountpoint/siblings/only-s3.txt"
zfs snapshot "$dataset@s3"
sha256sum "$mountpoint/payload/large.bin" | awk '{print $1}' > "$output_dir/s3.sha256"
stat -c '%s' "$mountpoint/payload/large.bin" > "$output_dir/s3.size"

zfs send "$dataset@s1" > "$output_dir/base.zfs"
zfs send -I "$dataset@s1" "$dataset@s3" > "$output_dir/increments.zfs"
cp "$output_dir/base.zfs" "$output_dir/history.zfs"
dd if="$output_dir/increments.zfs" of="$output_dir/history.zfs" \
    oflag=append conv=notrunc status=none

"$binary" snapshots "$output_dir/history.zfs"
for snapshot in s1 s2 s3; do
    target="$output_dir/large-$snapshot.bin"
    "$binary" extract "$output_dir/history.zfs" /payload/large.bin \
        --snapshot "$snapshot" --output "$target" --force
    test "$(stat -c '%s' "$target")" = "$(cat "$output_dir/$snapshot.size")"
    test "$(sha256sum "$target" | awk '{print $1}')" = \
        "$(cat "$output_dir/$snapshot.sha256")"
done

"$binary" list "$output_dir/history.zfs" /siblings --snapshot s2 | grep -q only-s2.txt
if "$binary" list "$output_dir/history.zfs" /siblings --snapshot s3 | grep -q only-s2.txt; then
    echo 'deleted s2-only file is unexpectedly present at s3' >&2
    exit 1
fi
"$binary" list "$output_dir/history.zfs" /siblings --snapshot s3 | grep -q only-s3.txt

printf 'verified 20 MiB snapshot selection at s1, s2, and s3\n'
