#!/bin/sh
set -eu

binary=${1:-target/release/zfs-send-extract}
dataset=${2:-labpool/zfs-send-encrypted}
output_dir=${3:-.artifacts/encrypted-verification}

if zfs list -H -o name "$dataset" >/dev/null 2>&1; then
    echo "dataset $dataset already exists; refusing to overwrite it" >&2
    exit 1
fi
if [ -e "$output_dir/raw-full.zfs" ] || [ -e "$output_dir/extracted.bin" ]; then
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
    -o compression=off \
    -o recordsize=128K \
    "$dataset"
mountpoint=$(zfs get -H -o value mountpoint "$dataset")
mkdir -p "$mountpoint/payload" "$mountpoint/siblings"
printf '%s\n' 'encrypted sibling' > "$mountpoint/siblings/unchanged.txt"

python3 - "$mountpoint/payload/large.bin" <<'PY'
import hashlib
import sys

path = sys.argv[1]
size = 20 * 1024 * 1024
with open(path, "wb") as output:
    counter = 0
    while output.tell() < size:
        block = hashlib.sha256(f"encrypted-raw:{counter}".encode()).digest() * 4096
        output.write(block[: min(len(block), size - output.tell())])
        counter += 1
PY

zfs snapshot "$dataset@s1"
zfs send -w "$dataset@s1" > "$output_dir/raw-full.zfs"
sha256sum "$mountpoint/payload/large.bin" | awk '{print $1}' > "$output_dir/expected.sha256"

"$binary" list "$output_dir/raw-full.zfs" /payload --key-file "$key_file"
"$binary" extract "$output_dir/raw-full.zfs" /payload/large.bin \
    --key-file "$key_file" --output "$output_dir/extracted.bin"

test "$(stat -c '%s' "$output_dir/extracted.bin")" = 20971520
test "$(sha256sum "$output_dir/extracted.bin" | awk '{print $1}')" = \
    "$(cat "$output_dir/expected.sha256")"

printf 'verified authenticated extraction of a 20 MiB AES-256-GCM raw send\n'
