#!/usr/bin/env bash
# Requires Linux OpenZFS, mke2fs, Python 3 and permission to create a file-backed pool.
# The supplied directory must be new. Images and the temporary dataset key
# remain there after export so that an unprivileged reader can test them.
set -euo pipefail
umask 077
fixture=${1:?usage: create-striped-pool-fixture.sh NEW_FIXTURE_DIRECTORY}
mkdir -- "$fixture"
fixture=$(realpath "$fixture")
mkdir "$fixture/members" "$fixture/mount"
pool=zfse_stripe_$(tr -d '-' </proc/sys/kernel/random/uuid | cut -c1-12)
printf '%s\n' "$pool" > "$fixture/pool-name"
left=$fixture/members/left.img
right=$fixture/members/right.img
truncate -s 128M "$left" "$right"
zpool create -o cachefile=none -o ashift=12 -O mountpoint=none -O compression=off -O atime=off "$pool" "$left" "$right"
trap 'zpool export "$pool"' EXIT
data=$fixture/mount/data
secure=$fixture/mount/secure
zfs create -o mountpoint="$data" -o recordsize=16K "$pool/files"
head -c 32 /dev/urandom > "$fixture/dataset.key"
zfs create -o mountpoint="$secure" -o encryption=aes-256-gcm -o keyformat=raw -o keylocation="file://$fixture/dataset.key" "$pool/locked"
mkdir -p "$data/documents/nested"
printf 'Before the change\n' > "$data/documents/revision.txt"
printf 'Nested history record\n' > "$data/documents/nested/note.txt"
printf 'Encrypted synthetic stripe record\n' > "$secure/private.txt"

# A raw ext4 disk nested in the ZFS filesystem, with an independent source tree.
mkdir -p "$fixture/guest-files/etc"
printf 'stripe-guest\n' > "$fixture/guest-files/etc/hostname"
(cd "$fixture/guest-files" && sha256sum etc/hostname) > "$fixture/nested.sha256"
truncate -s 16M "$data/guest.img"
mke2fs -q -t ext4 -F -d "$fixture/guest-files" "$data/guest.img"

# The repeated digest keeps exported test images inexpensive to transfer.
# Compression is disabled on both datasets to exercise real block allocation.
python3 - "$data/across.bin" <<'PY'
import hashlib
import os
import sys

with open(sys.argv[1], "wb") as f:
    for i in range(2048):
        content = hashlib.sha256(f"stripe-record-{i}".encode()).digest()
        f.write(content * 512)
        if i % 128 == 127:
            f.flush()
            os.fsync(f.fileno())
PY
zfs snapshot "$pool/files@early"
zfs snapshot "$pool/locked@early"
printf 'After the change\n' > "$data/documents/revision.txt"
printf 'Second snapshot offset changed\n' | dd of="$data/across.bin" bs=1 seek=16777216 conv=notrunc status=none
zfs snapshot "$pool/files@late"
zpool sync "$pool"

# Use the mounted OpenZFS filesystem independently of the recovery engine.
for snap in early late; do
    (cd "$data/.zfs/snapshot/$snap" && sha256sum across.bin documents/revision.txt documents/nested/note.txt) > "$fixture/$snap.sha256"
done
(cd "$secure/.zfs/snapshot/early" && sha256sum private.txt) > "$fixture/locked.sha256"
zpool status -P "$pool" > "$fixture/topology.txt"
zpool iostat -vp "$pool" > "$fixture/allocation.txt"
zfs list -r -t filesystem,snapshot "$pool" > "$fixture/snapshots.txt"
object=$(stat -c '%i' "$data/.zfs/snapshot/late/across.bin")
zdb -e -p "$fixture/members" -dddddd "$pool/files@late" "$object" > "$fixture/blocks.txt"
# Each file-data pointer has exactly one DVA. Require data on both disks,
# independently of which metadata copies the native reader chooses.
awk '
    $2 == "L0" && $4 == "4000L/4000P" {
        split($3, address, ":")
        blocks[address[1]]++
    }
    END {
        print "vdev 0:", blocks[0], "blocks"
        print "vdev 1:", blocks[1], "blocks"
        if (!blocks[0] || !blocks[1] || blocks[0] + blocks[1] != 2048) exit 1
    }
' "$fixture/blocks.txt" > "$fixture/block-distribution.txt"
zpool export "$pool"
trap - EXIT
(cd "$fixture" && sha256sum members/left.img members/right.img > members.sha256)
