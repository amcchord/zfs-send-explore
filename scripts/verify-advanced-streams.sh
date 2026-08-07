#!/bin/sh
set -eu

binary=${1:-target/release/zfs-send-extract}
pool=${2:-labpool}
output_dir=${3:-.artifacts/advanced-streams}
raw_dataset="$pool/zfse-raw-advanced"
plain_dataset="$pool/zfse-plain-advanced"

for command in zfs zstream setfattr getfattr python3 sha256sum; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "required command $command is unavailable" >&2
        exit 1
    fi
done
if ! zpool list -H -o name "$pool" >/dev/null 2>&1; then
    echo "pool $pool does not exist" >&2
    exit 1
fi
for dataset in "$raw_dataset" "$plain_dataset"; do
    if zfs list -H -o name "$dataset" >/dev/null 2>&1; then
        echo "dataset $dataset already exists; refusing to overwrite it" >&2
        exit 1
    fi
done
if [ -e "$output_dir/raw-full.zfs" ] || [ -e "$output_dir/plain-full.zfs" ]; then
    echo "verification outputs already exist in $output_dir; refusing to overwrite them" >&2
    exit 1
fi

mkdir -p "$output_dir"
output_dir=$(cd "$output_dir" && pwd)
key_file="$output_dir/public-test-passphrase"
printf '%s\n' 'zfs-send-fixture-passphrase' > "$key_file"
chmod 600 "$key_file"

zfs create \
    -o encryption=aes-256-gcm \
    -o keyformat=passphrase \
    -o keylocation="file://$key_file" \
    -o pbkdf2iters=100000 \
    -o compression=zstd-3 \
    -o recordsize=128K \
    -o xattr=sa \
    -o dnodesize=legacy \
    "$raw_dataset"
raw_mount=$(zfs get -H -o value mountpoint "$raw_dataset")
mkdir -p "$raw_mount/payload" "$raw_mount/siblings"
printf '%s\n' 'raw encrypted sibling' > "$raw_mount/siblings/unchanged.txt"

python3 - "$raw_mount/payload/target.bin" <<'PY'
import hashlib
import sys

path = sys.argv[1]
size = 2 * 1024 * 1024
with open(path, "wb") as output:
    counter = 0
    while output.tell() < size:
        seed = hashlib.sha256(f"raw-zstd:{counter // 4}".encode()).digest()
        block = seed * 4096
        output.write(block[: min(len(block), size - output.tell())])
        counter += 1
PY

attr_value=abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz
for number in $(seq 1 20); do
    setfattr -n "user.testattr$number" -v "$attr_value" \
        "$raw_mount/payload/target.bin"
done

zfs snapshot "$raw_dataset@s1"
zfs send -w "$raw_dataset@s1" > "$output_dir/raw-full.zfs"
sha256sum "$raw_mount/payload/target.bin" | awk '{print $1}' > "$output_dir/raw-s1.sha256"
stat -c '%s' "$raw_mount/payload/target.bin" > "$output_dir/raw-s1.size"

python3 - "$raw_mount/payload/target.bin" <<'PY'
import hashlib
import os
import sys

path = sys.argv[1]
with open(path, "r+b") as output:
    output.seek(128 * 1024)
    output.write(hashlib.sha256(b"raw-incremental-change").digest() * 2048)
    output.seek(0, os.SEEK_END)
    output.write(hashlib.sha256(b"raw-incremental-append").digest() * 8192)
PY
setfattr -x user.testattr1 "$raw_mount/payload/target.bin"
setfattr -n user.testattr21 -v 'changed spill attribute' "$raw_mount/payload/target.bin"
zfs snapshot "$raw_dataset@s2"
zfs send -w -i "$raw_dataset@s1" "$raw_dataset@s2" > "$output_dir/raw-incremental.zfs"
sha256sum "$raw_mount/payload/target.bin" | awk '{print $1}' > "$output_dir/raw-s2.sha256"
stat -c '%s' "$raw_mount/payload/target.bin" > "$output_dir/raw-s2.size"
cp "$output_dir/raw-full.zfs" "$output_dir/raw-history.zfs"
dd if="$output_dir/raw-incremental.zfs" of="$output_dir/raw-history.zfs" \
    oflag=append conv=notrunc status=none

"$binary" inspect "$output_dir/raw-full.zfs" | grep -E 'SPILL: [1-9][0-9]*'
"$binary" list "$output_dir/raw-full.zfs" /payload --key-file "$key_file"
"$binary" extract "$output_dir/raw-full.zfs" /payload/target.bin \
    --key-file "$key_file" --output "$output_dir/raw-target.bin"
test "$(stat -c '%s' "$output_dir/raw-target.bin")" = "$(cat "$output_dir/raw-s1.size")"
test "$(sha256sum "$output_dir/raw-target.bin" | awk '{print $1}')" = \
    "$(cat "$output_dir/raw-s1.sha256")"
"$binary" apply "$output_dir/raw-incremental.zfs" "$output_dir/raw-target.bin" \
    --key-file "$key_file"
test "$(stat -c '%s' "$output_dir/raw-target.bin")" = "$(cat "$output_dir/raw-s2.size")"
test "$(sha256sum "$output_dir/raw-target.bin" | awk '{print $1}')" = \
    "$(cat "$output_dir/raw-s2.sha256")"
"$binary" extract "$output_dir/raw-history.zfs" /payload/target.bin \
    --snapshot s2 --key-file "$key_file" --output "$output_dir/raw-target-s2.bin"
test "$(sha256sum "$output_dir/raw-target-s2.bin" | awk '{print $1}')" = \
    "$(cat "$output_dir/raw-s2.sha256")"

zfs create -o compression=zstd-3 -o recordsize=128K "$plain_dataset"
plain_mount=$(zfs get -H -o value mountpoint "$plain_dataset")
mkdir -p "$plain_mount/payload"
python3 - "$plain_mount/payload/target.bin" "$plain_mount/payload/embedded.bin" <<'PY'
import hashlib
import sys

target, embedded = sys.argv[1:]
def moderate_block(label):
    noisy = bytearray()
    counter = 0
    while len(noisy) < 64 * 1024:
        noisy.extend(hashlib.sha256(f"{label}:{counter}".encode()).digest())
        counter += 1
    return bytes(noisy[:64 * 1024]) + bytes(64 * 1024)

with open(target, "wb") as output:
    for block in range(16):
        output.write(moderate_block(f"plain-zstd-{block}"))
with open(embedded, "wb") as output:
    output.truncate(4096)
    output.seek(4088)
    output.write(b"embed-s1")
PY
zfs snapshot "$plain_dataset@s1"
zfs send -c -e "$plain_dataset@s1" > "$output_dir/plain-full.zfs"
sha256sum "$plain_mount/payload/target.bin" | awk '{print $1}' > "$output_dir/plain-s1.sha256"
sha256sum "$plain_mount/payload/embedded.bin" | awk '{print $1}' > "$output_dir/embed-s1.sha256"

python3 - "$plain_mount/payload/target.bin" "$plain_mount/payload/embedded.bin" <<'PY'
import hashlib
import os
import sys

target, embedded = sys.argv[1:]
def moderate_block(label):
    noisy = bytearray()
    counter = 0
    while len(noisy) < 64 * 1024:
        noisy.extend(hashlib.sha256(f"{label}:{counter}".encode()).digest())
        counter += 1
    return bytes(noisy[:64 * 1024]) + bytes(64 * 1024)

with open(target, "r+b") as output:
    output.seek(256 * 1024)
    output.write(moderate_block("compressed-incremental-change"))
    output.seek(0, os.SEEK_END)
    output.write(moderate_block("compressed-incremental-append"))
with open(embedded, "r+b") as output:
    output.seek(4088)
    output.write(b"embed-s2")
PY
zfs snapshot "$plain_dataset@s2"
zfs send -c -e -i "$plain_dataset@s1" "$plain_dataset@s2" > \
    "$output_dir/plain-incremental.zfs"
sha256sum "$plain_mount/payload/target.bin" | awk '{print $1}' > "$output_dir/plain-s2.sha256"
sha256sum "$plain_mount/payload/embedded.bin" | awk '{print $1}' > "$output_dir/embed-s2.sha256"
cp "$output_dir/plain-full.zfs" "$output_dir/plain-history.zfs"
dd if="$output_dir/plain-incremental.zfs" of="$output_dir/plain-history.zfs" \
    oflag=append conv=notrunc status=none

"$binary" inspect "$output_dir/plain-full.zfs" | grep -E 'WRITE_EMBEDDED: [1-9][0-9]*'
"$binary" extract "$output_dir/plain-full.zfs" /payload/target.bin \
    --output "$output_dir/plain-target.bin"
"$binary" extract "$output_dir/plain-full.zfs" /payload/embedded.bin \
    --output "$output_dir/embedded.bin"
test "$(sha256sum "$output_dir/plain-target.bin" | awk '{print $1}')" = \
    "$(cat "$output_dir/plain-s1.sha256")"
test "$(sha256sum "$output_dir/embedded.bin" | awk '{print $1}')" = \
    "$(cat "$output_dir/embed-s1.sha256")"
"$binary" apply "$output_dir/plain-incremental.zfs" "$output_dir/plain-target.bin"
"$binary" apply "$output_dir/plain-incremental.zfs" "$output_dir/embedded.bin"
test "$(sha256sum "$output_dir/plain-target.bin" | awk '{print $1}')" = \
    "$(cat "$output_dir/plain-s2.sha256")"
test "$(sha256sum "$output_dir/embedded.bin" | awk '{print $1}')" = \
    "$(cat "$output_dir/embed-s2.sha256")"
"$binary" extract "$output_dir/plain-history.zfs" /payload/target.bin \
    --snapshot s2 --output "$output_dir/plain-target-s2.bin"
test "$(sha256sum "$output_dir/plain-target-s2.bin" | awk '{print $1}')" = \
    "$(cat "$output_dir/plain-s2.sha256")"

printf 'verified raw incremental/spill/Zstandard and compressed/embedded replay streams\n'
