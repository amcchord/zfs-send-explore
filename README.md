# zfs-send-extract

`zfs-send-extract` is an experimental, pure-userspace CLI for browsing a ZFS send file and extracting one regular file without mounting a pool or installing ZFS. An extracted file carries a small JSON sidecar, allowing a later incremental send stream to update the file atomically.

The first milestone intentionally targets plain, single-filesystem send streams produced on little-endian Linux. It does not link `libzfs`, load a kernel module, invoke `zfs`, or receive the stream into a pool.

## Current commands

```text
zfs-send-extract inspect full.zfs
zfs-send-extract list full.zfs /
zfs-send-extract list full.zfs /payload
zfs-send-extract extract full.zfs /payload/large.bin --output large.bin
zfs-send-extract apply incremental.zfs large.bin
```

`extract` makes two streaming passes. The first retains only ZPL directory and System Attribute metadata so it can map a path to a DMU object. The second writes only that object's extents to the destination. It does not materialize every file in the stream.

`extract` also writes `large.bin.zfse.json`. The sidecar records the DMU object number, base snapshot GUID, logical size, metadata offset, and SHA-256. `apply` verifies both the GUID chain and the existing file hash before replaying only that object's `WRITE` and `FREE` records into a temporary file, then renames the result into place.

## Build

Rust 1.87 or later is required.

```sh
cargo build --release
cargo test
```

`tests/fixtures` contains a small pair of real OpenZFS 2.3.2 streams. The integration test exercises path listing, extraction, incremental update, and checksum-corruption rejection on every CI run. The 20+ MiB milestone fixture stays out of Git and can be verified on Linux with:

```sh
scripts/verify-fixtures.sh target/release/zfs-send-extract .artifacts/lab
```

The resulting executable has no ZFS runtime dependency:

```sh
ldd target/release/zfs-send-extract
```

## Supported stream profile

This initial version supports:

- a full `zfs send pool/fs@snapshot` stream for `list` and `extract`;
- a matching `zfs send -i pool/fs@base pool/fs@next` stream for `apply`;
- regular-file data represented by ordinary, uncompressed `WRITE` records;
- micro-ZAP and fat-ZAP directories; and
- modern SA metadata plus legacy znode bonuses.

It deliberately rejects:

- encrypted/raw (`zfs send -w`) streams;
- compressed (`zfs send -c`) and embedded-data (`zfs send -e`) streams;
- recursive/replication packages (`zfs send -R`), deduplicated, redacted, resumed, or big-endian streams; and
- an incremental update that deletes and recreates the file under a new DMU object number.

Dataset compression is fine: ordinary `zfs send` emits uncompressed replay payloads unless `-c` is explicitly requested.

## Reproducible ZFS fixture

The fixture producer is the only component that needs ZFS:

```sh
truncate -s 2G /var/tmp/zfs-send-lab.img
zpool create -f labpool /var/tmp/zfs-send-lab.img
scripts/create-fixtures.sh labpool/zfs-send-extract /root/zfs-send-fixtures
```

It creates a dataset containing several directories and files, including a deterministic 20 MiB target. It then changes three ranges, appends 2 MiB to the same inode, and emits full and incremental streams plus expected sizes and SHA-256 hashes.

## Design sources

The replay-record framing follows OpenZFS's public [`dmu_replay_record_t`](https://github.com/openzfs/zfs/blob/master/include/sys/zfs_ioctl.h) wire structure and Fletcher-4 stream semantics. ZPL ZAP and System Attribute decoding is provided by the Apache-2.0 [`zfs-forensic-core`](https://github.com/SecurityRonin/zfs-forensic) crate. See [docs/format-notes.md](docs/format-notes.md) for the mapping used by the implementation and [docs/test-evidence.md](docs/test-evidence.md) for the initial milestone results.

## License

Apache-2.0. See [LICENSE](LICENSE).
