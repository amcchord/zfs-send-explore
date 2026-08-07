# zfs-send-extract

`zfs-send-extract` is an experimental, pure-userspace CLI for browsing a ZFS send file and extracting one regular file without mounting a pool or installing ZFS. It supports ordinary plaintext sends and full OpenZFS raw encrypted sends (`zfs send -w`). It can also select a snapshot from compound or concatenated multi-snapshot send files. An extracted file carries a small JSON sidecar, allowing a later plaintext incremental send stream to update the file atomically.

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
```

`--snapshot` accepts a short snapshot name, a full `dataset@snapshot` name, or a `0x`-prefixed snapshot GUID. It is required when the file contains more than one snapshot, which avoids silently choosing the wrong dataset in a recursive package. The selected snapshot is reconstructed by following its `fromguid` ancestry back to a full substream and replaying that chain only.

For a multi-snapshot file, `extract` makes a catalog pass followed by two replay passes. The metadata pass retains only ZPL directory and System Attribute data so it can map a path to a DMU object. The final pass writes only that object's extents to the destination. It does not materialize every file in the stream.

`inspect` and `snapshots` are keyless for raw sends because the stream framing and snapshot names are not encrypted. `list` and `extract` prompt without echo for a passphrase or hex key when run in a terminal. `--key-file` is required for 32-byte binary raw keys and for non-interactive use. A passphrase/hex key file follows OpenZFS line semantics (one final newline is ignored); a raw key file must contain exactly 32 bytes. Keys are zeroized after use and are never stored in the extraction sidecar.

`extract` also writes `large.bin.zfse.json`. The sidecar records the DMU object number, base snapshot GUID, logical size, metadata offset, and SHA-256. `apply` verifies both the GUID chain and the existing file hash before replaying only that object's `WRITE` and `FREE` records into a temporary file, then renames the result into place.

## Build

Rust 1.87 or later is required.

```sh
cargo build --release
cargo test
```

`tests/fixtures` contains small real OpenZFS 2.3.2 streams, including a three-snapshot archive and an AES-256-GCM raw encrypted send. The integration tests exercise snapshot discovery and selection, path listing, extraction, encrypted key rejection, block authentication, incremental update, and checksum-corruption rejection on every CI run. The 20+ MiB milestone fixture stays out of Git and can be verified on Linux with:

```sh
scripts/verify-fixtures.sh target/release/zfs-send-extract .artifacts/lab
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
- uncompressed and LZ4-compressed blocks inside raw sends;
- micro-ZAP and fat-ZAP directories; and
- modern SA metadata plus legacy znode bonuses.

It deliberately rejects:

- raw encrypted incremental chains and `apply` on raw encrypted sends;
- raw encrypted spill blocks and raw compression algorithms other than off/LZ4;
- compressed (`zfs send -c`) and embedded-data (`zfs send -e`) streams;
- a selected incremental snapshot whose full base is not present earlier in the same file;
- deduplicated, redacted, resumed, or big-endian streams; and
- an incremental update that deletes and recreates the file under a new DMU object number.

Dataset compression is fine for ordinary sends: `zfs send` emits uncompressed replay payloads unless `-c` is explicitly requested. Raw sends always preserve their on-disk representation; this release can decompress the common off and LZ4 forms.

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
