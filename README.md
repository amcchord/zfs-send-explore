# zfs-send-extract

`zfs-send-extract` is an experimental, pure-userspace CLI for browsing ZFS send files and offline ZFS pool members without mounting a pool or installing ZFS. It supports ordinary plaintext sends, full OpenZFS raw encrypted sends (`zfs send -w`), and direct read-only extraction from a single disk/file vdev or one member of a single top-level mirror. It can select snapshots from compound send files or from the pool's on-disk DSL snapshot catalog. An extracted snapshot file carries a small JSON sidecar, allowing a later plaintext incremental send stream to update the file atomically.

The current profile targets streams produced on little-endian systems. It does not link `libzfs`, load a kernel module, invoke `zfs`, or receive the stream into a pool. Encrypted stream handling uses portable Rust cryptography and verifies the OpenZFS authentication tags before using decrypted file or path metadata.

## Current commands

```text
zfs-send-extract inspect full.zfs
zfs-send-extract snapshots history.zfs
zfs-send-extract list full.zfs /
zfs-send-extract list full.zfs /payload
zfs-send-extract extract full.zfs /payload/large.bin --output large.bin
zfs-send-extract extract history.zfs /payload/large.bin --snapshot daily-2026-08-01 --output older.bin
zfs-send-extract list encrypted-raw.zfs / --key-file ./dataset.key
zfs-send-extract extract encrypted-raw.zfs /payload/large.bin --key-file ./dataset.key --output large.bin
zfs-send-extract apply incremental.zfs large.bin

zfs-send-extract pool inspect /dev/sdb1
zfs-send-extract pool datasets /dev/sdb1
zfs-send-extract pool snapshots /dev/sdb1 tank/data
zfs-send-extract pool list /dev/sdb1 tank/data@s1 /payload
zfs-send-extract pool extract /dev/sdb1 tank/data@s1 /payload/large.bin --output large.bin
```

`--snapshot` accepts a short snapshot name, a full `dataset@snapshot` name, or a `0x`-prefixed snapshot GUID. It is required when the file contains more than one snapshot, which avoids silently choosing the wrong dataset in a recursive package. The selected snapshot is reconstructed by following its `fromguid` ancestry back to a full substream and replaying that chain only.

For a multi-snapshot file, `extract` makes a catalog pass followed by two replay passes. The metadata pass retains only ZPL directory and System Attribute data so it can map a path to a DMU object. The final pass writes only that object's extents to the destination. It does not materialize every file in the stream.

`inspect` and `snapshots` are keyless for raw sends because the stream framing and snapshot names are not encrypted. `list` and `extract` prompt without echo for a passphrase or hex key when run in a terminal. `--key-file` is required for 32-byte binary raw keys and for non-interactive use. A passphrase/hex key file follows OpenZFS line semantics (one final newline is ignored); a raw key file must contain exactly 32 bytes. Keys are zeroized after use and are never stored in the extraction sidecar.

`extract` also writes `large.bin.zfse.json`. The sidecar records the DMU object number, base snapshot GUID, logical size, metadata offset, and SHA-256. `apply` verifies both the GUID chain and the existing file hash before replaying only that object's `WRITE` and `FREE` records into a temporary file, then renames the result into place.

## Offline pool members

The `pool` command family reads an exported vdev with positioned I/O. It scans the four vdev labels, chooses the highest active uberblock, enforces every supported checksum present before decompression, walks the MOS and DSL dataset tree, and then uses the same ZPL ZAP/SA metadata model as send extraction. The source is always opened read-only and is not loaded into memory as one giant image.

Pass the exact ZFS vdev partition or file vdev. Whole disks containing GPT partitions are not auto-sliced yet. Export the pool first so blocks cannot be freed and reused while the reader is traversing a fixed uberblock:

```sh
zpool export tank
zfs-send-extract pool inspect /dev/disk/by-id/example-part1 --json
zfs-send-extract pool snapshots /dev/disk/by-id/example-part1 tank/data
zfs-send-extract pool extract /dev/disk/by-id/example-part1 \
  tank/data@backup-2026-08-07 /payload/large.bin --output large.bin
```

A healthy member of a mirror is sufficient only when that mirror is the pool's sole top-level vdev. A pool with several top-level mirrors needs one member from every mirror and is rejected by this release. Pools with RAIDZ/dRAID, special or dedup allocation classes, device-removal mappings, gang blocks, or encrypted datasets are also outside the initial direct-member profile.

Extraction from a named on-disk snapshot writes the normal `.zfse.json` sidecar using `dsl_dataset_phys_t.ds_guid`, so `apply` can advance that file with an ordinary incremental send. Extraction from a live head is supported but deliberately omits the sidecar because a head dataset is not a valid incremental-send base snapshot.

## Build

Rust 1.87 or later is required.

```sh
cargo build --release
cargo test
```

`tests/fixtures` contains small real OpenZFS 2.3.2 streams, including a three-snapshot archive and an AES-256-GCM raw encrypted send. The integration tests exercise snapshot discovery and selection, path listing, extraction, encrypted key rejection, block authentication, incremental update, positioned block reads, and checksum-corruption rejection on every CI run. The 20+ MiB milestone fixtures stay out of Git and can be verified on Linux with:

```sh
scripts/verify-fixtures.sh target/release/zfs-send-extract .artifacts/lab
scripts/verify-pool-member.sh target/release/zfs-send-extract \
  /path/to/exported-member.vdev tank/data@s1 /payload/large.bin \
  expected-s1-sha256 .artifacts/pool-member
```

The resulting executable has no ZFS runtime dependency:

```sh
ldd target/release/zfs-send-extract
```

The three-snapshot 20 MiB selection test can be reproduced on a Linux ZFS host with a fresh dataset name:

```sh
scripts/verify-snapshot-selection.sh \
  target/release/zfs-send-extract \
  labpool/zfs-send-snapshots \
  .artifacts/snapshot-selection
```

Authenticated extraction from a 20 MiB AES-256-GCM raw send can be reproduced on the same kind of host:

```sh
scripts/verify-encrypted-send.sh \
  target/release/zfs-send-extract \
  labpool/zfs-send-encrypted \
  .artifacts/encrypted-verification
```

## Supported stream profile

This initial version supports:

- a full `zfs send pool/fs@snapshot` stream for `list` and `extract`;
- OpenZFS compound framing used by `zfs send -I`, `-R`, and property sends;
- selecting a snapshot from a self-contained GUID chain in compound or concatenated send files;
- a matching `zfs send -i pool/fs@base pool/fs@next` stream for `apply`;
- regular-file data represented by ordinary, uncompressed `WRITE` records;
- a full raw encrypted `zfs send -w` snapshot using OpenZFS AES-CCM or AES-GCM (128, 192, or 256-bit), with raw, hex, or passphrase key formats;
- authenticated decryption of raw WRITE blocks and encrypted dnode bonus metadata;
- off, LZJB, LZ4, gzip, ZLE, and Zstandard-compressed blocks inside raw sends;
- micro-ZAP and fat-ZAP directories; and
- modern SA metadata plus legacy znode bonuses.

It deliberately rejects:

- raw encrypted incremental chains and `apply` on raw encrypted sends;
- raw encrypted spill blocks;
- compressed (`zfs send -c`) and embedded-data (`zfs send -e`) streams;
- a selected incremental snapshot whose full base is not present earlier in the same file;
- deduplicated, redacted, resumed, or big-endian streams; and
- an incremental update that deletes and recreates the file under a new DMU object number.

The direct pool-member backend additionally supports:

- exact file vdevs, ZFS partitions, and images opened read-only;
- a pool with one top-level `disk`, `file`, or `mirror` vdev;
- current filesystem datasets and named snapshots;
- Fletcher-2, Fletcher-4, and SHA-256 block validation;
- embedded blocks plus off, LZJB, LZ4, gzip levels 1-9, ZLE, and Zstandard compression; and
- compatible incremental-send sidecars for files extracted from named snapshots.

Dataset compression is fine for ordinary sends: `zfs send` emits uncompressed replay payloads unless `-c` is explicitly requested. Direct pool-member reads and raw encrypted sends share an OpenZFS-aware decoder for off, LZJB, LZ4, gzip levels 1-9, ZLE, and Zstandard, including ZFS's magicless Zstandard framing. Plaintext compressed replay records produced by `zfs send -c` remain outside the current send-stream profile.

The pool reader targets ZFS filesystem datasets. ZVOLs are block devices rather than path-based filesystems, so their contents cannot be browsed as individual files without also understanding the filesystem stored inside the volume.

An important OpenZFS detail is that `zfs send -I pool/fs@s1 pool/fs@s3` includes `s2` and `s3`, but not the starting snapshot `s1`. That stream alone is not sufficient to reconstruct file data. A self-contained history file can be made by concatenating the full base and the compound incremental stream:

```sh
zfs send pool/fs@s1 > base.zfs
zfs send -I pool/fs@s1 pool/fs@s3 > increments.zfs
cat base.zfs increments.zfs > history.zfs

zfs-send-extract snapshots history.zfs
zfs-send-extract extract history.zfs /path/to/file --snapshot s2 --output file-at-s2
```

## Reproducible ZFS fixture

The fixture producer is the only component that needs ZFS:

```sh
truncate -s 2G /var/tmp/zfs-send-lab.img
zpool create -f labpool /var/tmp/zfs-send-lab.img
scripts/create-fixtures.sh labpool/zfs-send-extract /root/zfs-send-fixtures
```

It creates a dataset containing several directories and files, including a deterministic 20 MiB target. It then changes three ranges, appends 2 MiB to the same inode, and emits full and incremental streams plus expected sizes and SHA-256 hashes.

## Design sources

The replay-record framing follows OpenZFS's public [`dmu_replay_record_t`](https://github.com/openzfs/zfs/blob/master/include/sys/zfs_ioctl.h) wire structure and Fletcher-4 stream semantics. Raw key unwrapping and block authentication follow OpenZFS [`zio_crypt.c`](https://github.com/openzfs/zfs/blob/master/module/os/linux/zfs/zio_crypt.c) and [`dsl_crypt.c`](https://github.com/openzfs/zfs/blob/master/module/zfs/dsl_crypt.c). ZPL ZAP and System Attribute decoding is provided by the Apache-2.0 [`zfs-forensic-core`](https://github.com/SecurityRonin/zfs-forensic) crate. See [docs/format-notes.md](docs/format-notes.md) for the mapping used by the implementation and [docs/test-evidence.md](docs/test-evidence.md) for the initial milestone results.

## License

Apache-2.0. See [LICENSE](LICENSE).
