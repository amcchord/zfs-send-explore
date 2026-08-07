#!/bin/sh
set -eu

binary=${1:-target/release/zfs-send-extract}
fixture_dir=${2:-.artifacts/lab}
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/zfs-send-extract-verify.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT HUP INT TERM

target="$work_dir/large.bin"

"$binary" inspect "$fixture_dir/full.zfs" >/dev/null
"$binary" inspect "$fixture_dir/incremental.zfs" >/dev/null
"$binary" list "$fixture_dir/full.zfs" / | grep -q 'payload'
"$binary" list "$fixture_dir/full.zfs" /payload | grep -q 'large.bin'
"$binary" extract "$fixture_dir/full.zfs" /payload/large.bin --output "$target"

test "$(stat -c '%s' "$target")" = "$(cat "$fixture_dir/base.size")"
test "$(sha256sum "$target" | awk '{print $1}')" = "$(cat "$fixture_dir/base.sha256")"

"$binary" apply "$fixture_dir/incremental.zfs" "$target"
test "$(stat -c '%s' "$target")" = "$(cat "$fixture_dir/incremental.size")"
test "$(sha256sum "$target" | awk '{print $1}')" = "$(cat "$fixture_dir/incremental.sha256")"

printf 'verified full extraction and incremental update\n'

