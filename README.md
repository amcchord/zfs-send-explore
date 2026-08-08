# zfs-send-extract

`zfs-send-extract` is an experimental, pure-userspace toolkit for browsing and extracting individual files from ZFS backups without importing or mounting a pool. It includes the cross-platform CLI and an additional native Windows desktop client. Both work with ZFS send files and, within a deliberately narrow pool layout, exported ZFS vdev members or images.

The extraction machine does **not** need ZFS, `libzfs`, or a ZFS kernel module. The tool never invokes `zfs` or `zpool`, and pool members are opened read-only.

> [!IMPORTANT]
> This is an early `0.4.0` implementation. The CLI, Windows UI, sidecar format, and supported on-disk profile may change. Stream and native-encryption pool fixtures are produced on little-endian OpenZFS systems. Linux validates the core end to end, and CI runs the full test suite and release packaging on native Windows.

Detailed implementation and validation material lives in:

- [`docs/format-notes.md`](docs/format-notes.md), which maps the supported send and on-disk formats to the reader; and
- [`docs/test-evidence.md`](docs/test-evidence.md), which records the real OpenZFS fixtures, lab scenarios, sizes, and hashes.
- [`docs/windows-client.md`](docs/windows-client.md), which covers the Windows UI, attached-drive access, updates, sparse files, and packaging.

## What works today

| Source or operation | Status |
| --- | --- |
| Inspect a full or incremental send stream | Supported |
| Browse and extract a regular file from a plaintext full send | Supported |
| Select a snapshot from a self-contained compound or concatenated send file | Supported |
| Update an extracted file with a matching plaintext incremental send | Supported |
| Browse and extract from a full raw encrypted send (`zfs send -w`) | Supported |
| Select from a self-contained raw encrypted incremental chain | Supported |
| Apply a matching raw encrypted incremental send | Supported with the extraction key and raw sidecar state |
| Authenticate and decode raw encrypted SA spill blocks | Supported |
| Read compressed blocks in raw sends | LZJB, LZ4, gzip, ZLE, and Zstandard |
| Read compressed replay records from `zfs send -c` | Supported |
| Read embedded-data replay records from `zfs send -e` | Supported |
| Browse a single exported disk/file vdev or one leaf of a single top-level mirror | Supported |
| Browse current datasets and named snapshots directly from a pool member | Supported |
| Explore a raw or sparse disk image stored as a regular ZFS file | Supported, including explicit byte offset/length windows; validated across every subordinate filesystem below |
| Explore self-contained QCOW2 and VMDK sparse containers stored on ZFS | QCOW2 v2/v3 and VMDK `monolithicSparse`; sparse QCOW2 v3 and VMDK are in the full matrix |
| Browse and extract from a subordinate filesystem (“inception mode”) | NTFS, FAT12/16/32, exFAT, ext4, and compatible ext2; all seven are in the full matrix |
| Native Windows snapshot browser and extractor | Supported as `zfs-send-explore-windows.exe` |
| Open a GPT/MBR whole-disk image or `\\.\PhysicalDriveN` | Supported when exactly one partition resolves to a supported plain or LUKS-wrapped ZFS member |
| Preserve sparse holes during extraction and incremental updates | Supported; zero ranges are deallocated when the destination filesystem permits it |
| Read compressed and embedded blocks from a pool member | LZJB, LZ4, gzip, ZLE, and Zstandard |
| Extract from a native-encrypted pool dataset | AES-CCM/GCM at 128/192/256 bits with raw, hex, or passphrase keys on little-endian pools |
| Open a Slide Box backup | Supported through the native-encrypted pool and nested raw-disk readers; Slide's 64-character raw-key representation is accepted directly |
| Open a Datto Reverse RoundTrip drive | Built-in read-only LUKS1/LUKS2 AES-XTS source; no `cryptsetup`, driver, or host mount required |
| Browse Datto `.datto` and encrypted `.detto` volumes | Supported; `.detto` keys are authenticated and derived from `.encryptionKeyStash` plus the agent password |
| Recursively recover a folder | Supported for send snapshots, pool datasets/snapshots, and subordinate filesystems; symlinks and special entries are not followed |
| Read RAIDZ/dRAID or pools with several top-level vdevs | Not yet supported |

The tool extracts individual regular files or recursively stages a selected directory tree. Recursive recovery never follows symlinks or special entries and does not recreate ownership, permissions, ACLs, alternate NTFS streams, or other filesystem metadata.

## Downloads

Portable release builds are available from the [GitHub Releases page](https://github.com/amcchord/zfs-send-explore/releases/latest). No installer is required.

| Asset | Contents |
| --- | --- |
| `zfs-send-extract-linux-x86_64.tar.gz` | Linux x86-64 command-line client |
| `zfs-send-extract-windows-x86_64.exe` | Windows x86-64 command-line client |
| `zfs-send-explore-windows-x86_64.exe` | Native Windows x86-64 desktop client |
| `zfs-send-explore-windows-x86_64.zip` | Both Windows executables, the illustrated Windows guide, screenshots, README, and license |
| `SHA256SUMS.txt` | SHA-256 checksums for every downloadable program and archive |

Verify a Windows download before running it:

```powershell
Get-FileHash .\zfs-send-explore-windows-x86_64.exe -Algorithm SHA256
```

On Linux, extract the CLI archive and run it directly:

```sh
tar -xzf zfs-send-extract-linux-x86_64.tar.gz
./zfs-send-extract --help
```

The Windows executables are currently unsigned, so Microsoft Defender SmartScreen may display an unrecognized-app warning. Check the release checksum and repository source before choosing to run the program.

## Build

Rust 1.87 or later is required:

```sh
git clone https://github.com/amcchord/zfs-send-explore.git
cd zfs-send-explore
cargo build --release
```

The executable is written to `target/release/zfs-send-extract`. ZFS is needed only on a system that creates send files or test fixtures; it is not a runtime dependency of the CLI.

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

### Native Windows client

![The native Windows client selecting a snapshot from a compound send](docs/screenshots/windows/snapshot-selector.jpg)

On Windows 10 or 11, build the GUI with the stable MSVC Rust toolchain and the Windows SDK:

```powershell
cargo build --release --bin zfs-send-explore-windows
.\scripts\package-windows.ps1
```

The GUI binary is `target\release\zfs-send-explore-windows.exe`; the packaging script also builds `zfs-send-extract.exe` and creates a distributable ZIP containing both clients and the illustrated documentation. It uses Win32 common controls, Windows file dialogs, the Segoe UI system typeface, per-monitor DPI scaling, and background workers so long stream scans do not block the window.

Open a send file or vdev image with **Browse**, or type a raw device path such as `\\.\PhysicalDrive3` and choose **Open pool / drive**. Physical-drive access normally requires starting the client as Administrator. The reader opens the source read-only, validates GPT metadata when auto-selecting a partition, and never imports or mounts the pool. See [the Windows client guide](docs/windows-client.md) for the complete workflow and safety constraints.

## Browse and extract from a send file

Inspect the stream and discover the snapshots it contains:

```sh
zfs-send-extract inspect backup.zfs
zfs-send-extract inspect backup.zfs --json
zfs-send-extract snapshots backup.zfs
```

List a directory and extract one file:

```sh
zfs-send-extract list backup.zfs /
zfs-send-extract list backup.zfs /payload
zfs-send-extract extract backup.zfs /payload/large.bin --output large.bin
```

Paths inside ZFS are absolute. Existing output files are preserved unless `--force` is supplied.

### Select a snapshot

When a send file contains more than one snapshot, select one by short name, full `dataset@snapshot` name, or `0x`-prefixed snapshot GUID:

```sh
zfs-send-extract snapshots history.zfs
zfs-send-extract list history.zfs /payload --snapshot s2
zfs-send-extract extract history.zfs /payload/large.bin \
  --snapshot tank/data@s2 \
  --output large-at-s2.bin
```

Selection is required when several snapshots are present, preventing the CLI from silently choosing the wrong dataset in a recursive package. The chosen snapshot must have a complete ancestry chain back to a full substream in the same file.

For example, `zfs send -I tank/data@s1 tank/data@s3` contains `s2` and `s3`, but not its starting snapshot `s1`. Make a self-contained history by concatenating the full base and compound incrementals:

```sh
zfs send tank/data@s1 > base.zfs
zfs send -I tank/data@s1 tank/data@s3 > increments.zfs
cat base.zfs increments.zfs > history.zfs

zfs-send-extract extract history.zfs /payload/large.bin \
  --snapshot s2 \
  --output large-at-s2.bin
```

The catalog and metadata passes retain only the information needed to resolve the requested path. The data pass writes only the selected object's extents; it does not receive the dataset or materialize every file in the stream.

## Apply an incremental send

Every successful send-stream extraction writes a JSON sidecar beside the output. For `large.bin`, the sidecar is `large.bin.zfse.json`. It records the DMU object, snapshot GUID, logical size, relevant metadata offset, and SHA-256 hash.

Use a matching ordinary incremental send to advance the file:

```sh
zfs send tank/data@s1 > full-s1.zfs
zfs send -i tank/data@s1 tank/data@s2 > s1-to-s2.zfs

zfs-send-extract extract full-s1.zfs /payload/large.bin --output large.bin
zfs-send-extract apply s1-to-s2.zfs large.bin
```

Before applying changes, the CLI verifies the sidecar's base GUID, the target's current size, and its SHA-256 hash. It replays matching object and spill metadata, ordinary or compressed writes, embedded writes, and free records into a temporary file. Only after validation succeeds does it atomically replace the target and advance the sidecar to the new snapshot and hash.

The update fails safely if the file has been modified locally, the incremental stream starts from another snapshot, or the file was deleted and recreated under a different DMU object number.

## Raw encrypted sends

Full and incremental raw sends produced by `zfs send -w` can be inspected without ZFS. Stream framing and snapshot names remain visible, so `inspect` and `snapshots` do not require a key. File paths, metadata, and contents require the dataset key:

```sh
zfs-send-extract snapshots encrypted-raw.zfs
zfs-send-extract list encrypted-raw.zfs / --key-file ./dataset.key
zfs-send-extract extract encrypted-raw.zfs /payload/large.bin \
  --key-file ./dataset.key \
  --output large.bin
zfs-send-extract apply encrypted-incremental.zfs large.bin \
  --key-file ./dataset.key
```

When attached to a terminal, `list`, `extract`, and raw `apply` prompt without echo for passphrase and hex keys if `--key-file` is omitted. Non-interactive use requires `--key-file`. Supported OpenZFS key formats are:

- passphrase, with one final newline in the key file ignored;
- hex key, with one final newline ignored; and
- raw key, which must be exactly 32 binary bytes and always uses `--key-file`.

OpenZFS AES-CCM and AES-GCM suites at 128, 192, and 256 bits are supported. Wrapped-key authentication, block tags, authenticated metadata, SA spill blocks, and the indirect checksum-of-MAC tree are verified before decrypted data is used. Key material is zeroized after use and is never stored in the extraction sidecar.

Selecting a raw incremental snapshot from a history file follows the same rule as plaintext selection: the full raw base and every required incremental substream must occur earlier in the same file. The reader retains the raw block-pointer authentication state needed to validate each updated 32-slot dnode range.

A raw extraction sidecar additionally stores the portable dnode and block-MAC state for the target's range. This lets `apply` authenticate a standalone raw incremental without retaining the full base stream. The sidecar may be larger than a plaintext sidecar for files with many blocks, but it contains no encryption key. Raw `apply` validates the key identity, base snapshot GUID, file hash, individual target blocks, and the updated dnode-range authentication tag before replacing the target.

## Recover Slide Box and Datto Reverse RoundTrip backups

The vendor workflows use the same read-only pool and inception engines rather
than shelling out to recovery utilities. All decryption and filesystem access is
compiled into the CLI and Windows application.

### Slide Box

A Slide Box pool is recognized by its `slide` pool name. Agent datasets such as
`slide/agents/a_abc123` appear as selectable pool views, and their `disk_*.raw`
files can be explored directly as virtual disks:

```powershell
zfs-send-extract pool inspect '\\.\PhysicalDrive3'
zfs-send-extract pool datasets '\\.\PhysicalDrive3'

zfs-send-extract pool inception inspect '\\.\PhysicalDrive3' `
  slide/agents/a_abc123 /disk_example.raw `
  --key-file .\slide-agent-key.txt

zfs-send-extract pool inception extract-tree '\\.\PhysicalDrive3' `
  slide/agents/a_abc123 /disk_example.raw /Users `
  --volume gpt3 --key-file .\slide-agent-key.txt `
  --output .\Recovered-Users
```

For a dataset whose OpenZFS `keyformat` is `raw`, `--key-file` accepts either
the exact 32 binary bytes or the 64 hexadecimal characters supplied by Slide
Support, with an optional final newline. The app performs the conversion in
memory; `xxd` is not required.

### Datto Reverse RoundTrip

A whole Reverse RoundTrip disk may use GPT or MBR and contains a LUKS-encrypted
partition around its ZFS pool. Supply the pool passphrase in a text file. The
LUKS1/LUKS2 reader derives and decrypts sectors on demand in userspace:

```powershell
zfs-send-extract pool inspect '\\.\PhysicalDrive4' `
  --container-key-file .\datto-pool-passphrase.txt

zfs-send-extract pool datasets '\\.\PhysicalDrive4' `
  --container-key-file .\datto-pool-passphrase.txt
```

An unencrypted `.datto` volume then follows the ordinary inception workflow:

```powershell
zfs-send-extract pool inception inspect '\\.\PhysicalDrive4' `
  revRT-123456/home/agents/AGENT_GUID /VOLUME_GUID.datto `
  --container-key-file .\datto-pool-passphrase.txt
```

For `.detto`, additionally supply the protected system's agent password. The
reader locates the single `config/*.encryptionKeyStash` file automatically,
authenticates its compact JWE with PBKDF2-SHA3-256 and AES-256-GCM, and exposes
the AES-256-XTS volume as another positioned virtual disk:

```powershell
zfs-send-extract pool inception extract-tree '\\.\PhysicalDrive4' `
  revRT-123456/home/agents/AGENT_GUID /VOLUME_GUID.detto /Users `
  --container-key-file .\datto-pool-passphrase.txt `
  --agent-password-file .\datto-agent-password.txt `
  --volume gpt1 --output .\Recovered-Users
```

Use `--key-stash /config/name.encryptionKeyStash` only when the agent has more
than one matching stash. Passwords are read from bounded files with a final
CR/LF removed; they are never accepted as command-line values or written to an
extraction sidecar.

Both vendor paths retain the direct-pool layout limits below. In particular, a
Slide or Datto pool using RAIDZ, several top-level vdevs, or unsupported
allocation classes still requires a future multi-member pool reader.

## Browse an offline pool member

The `pool` command family reads an exported vdev using positioned I/O. Pass an exact ZFS partition, file vdev, or image. A GPT/MBR whole-disk image is also accepted when exactly one partition resolves to a supported plain or LUKS-wrapped ZFS member; the Windows client uses the same discovery for `\\.\PhysicalDriveN`.

Export a live pool first. This keeps the selected uberblock stable and prevents blocks from being freed and reused during traversal:

```sh
zpool export tank

zfs-send-extract pool inspect /dev/disk/by-id/example-part1 --json
zfs-send-extract pool datasets /dev/disk/by-id/example-part1
zfs-send-extract pool snapshots /dev/disk/by-id/example-part1 tank/data
zfs-send-extract pool list /dev/disk/by-id/example-part1 \
  tank/data@backup-2026-08-07 /payload
zfs-send-extract pool extract /dev/disk/by-id/example-part1 \
  tank/data@backup-2026-08-07 /payload/large.bin \
  --output large.bin
```

Native-encrypted dataset heads and snapshots use the same commands. Supply a
key file, or omit it in an interactive terminal to receive a no-echo prompt:

```sh
zfs-send-extract pool list encrypted-member.img tank/private@backup / \
  --key-file ./tank-private.key
zfs-send-extract pool extract encrypted-member.img tank/private@backup \
  /payload/large.bin --key-file ./tank-private.key --output large.bin
```

The reader follows `com.datto:crypto_key_obj` through the DSL parent chain, so
an encrypted child can use its inherited encryption-root key. The same raw,
hex, and passphrase key-file rules and AES-CCM/GCM suites documented for raw
sends apply. Wrapped keys, the objset portable MAC, authenticated metadata,
encrypted dnode bonuses, encrypted file and directory blocks, truncated
physical checksums, and indirect checksum-of-MAC trees are validated before
plaintext is used. Key material is zeroized after the operation and never
written to a sidecar.

The reader scans all four vdev labels, selects the highest active uberblock, validates supported checksums before decompression, walks the MOS and DSL dataset tree, and resolves paths through ZPL ZAP and System Attribute metadata. It does not load the entire member into memory.

Current pool-layout support is intentionally conservative:

- one top-level disk or file vdev; or
- one top-level mirror, using either healthy leaf independently.

A pool containing multiple top-level vdevs needs members from each top-level vdev and is rejected because the CLI currently accepts only one source. RAIDZ/dRAID, special or dedup allocation classes, device-removal mappings, and gang blocks are also rejected explicitly. Native-encrypted datasets are supported on little-endian pools; encrypted big-endian pools are rejected explicitly until their byte-swapped authentication path is validated.

Extracting from a named pool snapshot writes the same incremental-compatible `.zfse.json` sidecar used by send extraction. Extracting from a current dataset head is supported, but it intentionally removes or omits the sidecar because a live head is not a valid incremental-send base snapshot.

The pool reader targets ZFS filesystem datasets. ZVOLs contain a block device rather than a ZPL path tree and therefore cannot be browsed as individual files by this tool.

## Explore a filesystem inside a ZFS file

Inception mode treats one regular file in a ZFS filesystem as a read-only virtual disk. It follows the layers directly—ZFS file extents, an optional sparse-disk container, a partition, and the subordinate filesystem—without first exporting the complete image or mounting it on the host.

Inspect an image in a send-stream snapshot before browsing it:

```sh
zfs-send-extract inception inspect backup.zfs /vms/server.qcow2 \
  --snapshot tank/vms@nightly

zfs-send-extract inception list backup.zfs /vms/server.qcow2 / \
  --snapshot tank/vms@nightly --volume gpt2

zfs-send-extract inception extract backup.zfs /vms/server.qcow2 \
  /Windows/System32/config/SYSTEM --snapshot tank/vms@nightly \
  --volume gpt2 --output SYSTEM
```

The same operations work against a current dataset or named snapshot in an offline pool member:

```sh
zfs-send-extract pool inception inspect member.img tank/vms@nightly /images/server.vmdk
zfs-send-extract pool inception list member.img tank/vms@nightly \
  /images/server.vmdk /etc --volume gpt1
zfs-send-extract pool inception extract member.img tank/vms@nightly \
  /images/server.vmdk /etc/hostname --volume gpt1 --output hostname
```

Add `--key-file ./dataset.key` to any `pool inception` command when its outer
dataset or snapshot is native-encrypted.

`inspect` reports the container, virtual disk size, partition selectors, byte ranges, filesystem types, labels, and a diagnostic for every unrecognized partition. When exactly one supported volume exists, `--volume` can be omitted; otherwise selection is required so the tool never guesses. `--json` makes inspection scriptable.

For an image embedded after an appliance header or inside a larger sparse file, provide a bounded byte window. Decimal, hexadecimal, and underscore-grouped values are accepted:

```sh
zfs-send-extract inception inspect backup.zfs /appliance/blob.bin \
  --image-offset 0x10_0000 --image-length 8_589_934_592
```

The layer support is deliberately explicit:

| Layer | Read-only support |
| --- | --- |
| Raw/sparse image | Unpartitioned filesystem or MBR/GPT disk; absent ZFS extents read as zeroes |
| QCOW | Self-contained QCOW2 v2/v3, including sparse, zero, and deflate-compressed clusters |
| VMDK | Self-contained `monolithicSparse` sparse extent |
| Partition table | MBR primary and bounded EBR logical partitions; GPT with header and entry CRC validation, 512/4096-byte sectors, and backup-header recovery |
| NTFS | NTFS 3.x directory browsing and extraction of the unnamed regular-file data stream |
| FAT | FAT12, FAT16, FAT32, and exFAT directory browsing and regular-file extraction |
| ext | ext4 and compatible ext2 directory browsing and regular-file extraction |

### What the inception release gate validates

The v0.3.0 release gate runs a 63-case cross-product: FAT12, FAT16, FAT32, exFAT, NTFS, ext4, and compatible ext2 are each tested unpartitioned, in MBR, and in GPT; every resulting disk is then tested as raw, sparse QCOW2 v3, and VMDK `monolithicSparse`. Each case detects the layers, lists a real directory, resolves a nested path, extracts through an explicit volume selector, and compares exact bytes.

Focused tests additionally validate resident, non-resident, and sparse NTFS data; a 512-entry NTFS directory index; FAT/exFAT long names, nested paths, and case-insensitive lookup; ext2/ext4 holes; and refusal to follow ext symlinks. Corruption tests cover QCOW1, QCOW2 backing files and encryption, external VMDK descriptors, invalid and backup GPT metadata, out-of-bounds MBR entries, looping EBR chains, unknown filesystems, unsafe paths, multiple-volume selection, and explicit container windows.

QCOW2 v2, deflate-compressed QCOW2 clusters, and 4096-byte-sector GPT are implemented reader profiles but are not members of the 63-case cross-product. The Windows UI service layer has automated list/extract coverage and the complete suite runs on native Windows CI; this is distinct from a scripted interactive UI walkthrough for every matrix case. Fixture provenance and hashes are recorded in [`tests/fixtures/inception/README.md`](tests/fixtures/inception/README.md), with detailed results in [`docs/test-evidence.md`](docs/test-evidence.md).

QCOW1, QCOW2 overlays that require a backing file, encrypted QCOW2, multi-file/split/flat/stream-optimized VMDK, NTFS-compressed or EFS-encrypted data, and non-UTF-8 ext names are reported rather than silently misread. ext symlinks are listed but never followed during extraction. Recursive inception extraction recreates directories and regular-file contents but skips symlinks and special entries and does not recreate inner ACLs, alternate NTFS streams, ownership, or permissions.

For send streams, the selected snapshot chain is scanned once when the image is opened to build a compact map from virtual ZFS-file ranges to replay payloads. Reads then decode only the blocks requested by the partition and filesystem readers, with a one-block cache. Pool-member reads similarly fetch only the addressed ZFS blocks. Sparse virtual disk capacity therefore does not become an equivalent RAM or temporary-disk requirement.

The ZFS source, container, partition, and subordinate filesystem are never written. A recovered inner file uses the ordinary extraction path: a same-directory temporary file, sparse writes, synchronization, atomic replacement, and SHA-256. It intentionally has no `.zfse.json` incremental-update sidecar because it is not itself a ZFS object.

## Compression support

Dataset compression does not normally make a plaintext send incompatible: without `zfs send -c`, OpenZFS emits logical, uncompressed replay payloads. Compressed replay records emitted by `zfs send -c` are also decoded directly.

Raw encrypted sends and direct pool-member reads preserve on-disk compression. These paths share a pure-Rust decoder for:

- compression off;
- LZJB;
- LZ4;
- gzip levels 1 through 9;
- ZLE; and
- Zstandard, including OpenZFS's magicless Zstandard frame.

The send reader handles `WRITE_EMBEDDED` records emitted by `zfs send -e`, including their eight-byte wire padding and logical block zero-fill semantics. OpenZFS does not permit `WRITE_EMBEDDED` records in raw streams; raw sends represent protected blocks with ordinary raw `WRITE` records instead. The pool backend independently handles embedded block pointers on disk.

## Integrity and safety model

The CLI treats stream and pool metadata as untrusted input and fails closed when it encounters an unsupported or inconsistent layout:

- send records and terminal stream checksums are validated, and truncated streams are rejected;
- raw-send and native-pool wrapped keys, block tags, authenticated metadata, spill blocks, dnode-range tags, and objset portable MACs are verified before decrypted bytes are used;
- supported pool block checksums, including the truncated/folded checksums used beside native-encryption MACs, are verified before decompression, with alternate DVA copies tried when present;
- pool members are never written, imported, or mounted; and
- extraction and incremental updates are built in a temporary file beside the destination, synchronized, and atomically renamed only after validation completes.

The `.zfse.json` sidecar is part of an extracted file's update state. Keep it beside the file and do not edit it. Raw sidecars contain portable block-authentication metadata but never the encryption key; they may still reveal path, object, size, and snapshot information. Passphrases committed under `tests/fixtures` are intentionally public test credentials and must never be reused for real datasets.

Extraction marks Windows destination files sparse and replays ZFS holes without materializing their zero bytes. Incremental updates copy only allocated source ranges when the host filesystem provides that information, and FREE records deallocate ranges on NTFS/ReFS, Linux filesystems with hole punching, and APFS. On filesystems without sparse controls, the logical content remains correct but zero ranges may consume physical space.

This remains experimental software, not a replacement for maintaining independently verified backups. Preserve the original send file or pool member until the extracted result has been checked for the intended recovery use.

## Command reference

| Command | Purpose |
| --- | --- |
| `inspect <stream> [--json]` | Validate framing and checksums; summarize snapshots and replay records |
| `snapshots <stream>` | List the full/incremental snapshots contained in a send file |
| `list <stream> [path] [--snapshot ...] [--key-file ...]` | List one directory in a selected snapshot |
| `extract <stream> <path> -o <file> [--snapshot ...] [--key-file ...] [--force]` | Extract one regular file and write its sidecar |
| `extract-tree <stream> <path> -o <directory> [...]` | Recursively stage and publish one directory tree without per-file sidecars |
| `apply <incremental-stream> <target> [--key-file ...]` | Atomically update a previously extracted file; the key is required for raw sends |
| `inception inspect <stream> <image> [--snapshot ...] [--image-offset ...] [--image-length ...] [--json]` | Detect a nested container, partitions, and subordinate filesystems |
| `inception list <stream> <image> [path] [--volume ...] [...]` | List a directory inside a selected subordinate filesystem |
| `inception extract <stream> <image> <path> -o <file> [--volume ...] [...]` | Extract one regular file from a subordinate filesystem |
| `inception extract-tree <stream> <image> <path> -o <directory> [...]` | Recursively recover a subordinate directory tree |
| `pool inspect <member> [--json] [--container-key-file ...]` | Validate a vdev member, including a built-in LUKS source, and summarize its active pool state |
| `pool datasets <member>` | List reachable filesystem datasets |
| `pool snapshots <member> [dataset]` | List named snapshots, optionally for one dataset |
| `pool list <member> <dataset[@snapshot]> [path] [--key-file ...]` | List one directory from a dataset head or snapshot |
| `pool extract <member> <dataset[@snapshot]> <path> -o <file> [--key-file ...] [--force]` | Extract one regular file directly from the member |
| `pool extract-tree <member> <dataset[@snapshot]> <path> -o <directory> [...]` | Recursively recover a ZFS directory tree |
| `pool inception inspect/list/extract/extract-tree ...` | Explore `.raw`, `.datto`, `.detto`, QCOW2, or VMDK files; Datto adds `--container-key-file` and, for `.detto`, `--agent-password-file` |

Run `zfs-send-extract <command> --help` for complete argument details.

## Supported stream profile and limitations

The current send reader supports:

- full plaintext `zfs send pool/fs@snapshot` streams;
- OpenZFS compound framing used by `zfs send -I`, `-R`, and property sends;
- snapshot selection from a self-contained GUID chain in compound or concatenated files;
- one matching plaintext `zfs send -i` substream for `apply`;
- compressed ordinary `WRITE` records from `zfs send -c`;
- embedded-data `WRITE_EMBEDDED` records from `zfs send -e`;
- full and incremental raw encrypted snapshots with authenticated raw `WRITE`, dnode bonus, and spill metadata;
- micro-ZAP and fat-ZAP directories; and
- modern SA metadata and legacy znode bonuses.

It deliberately rejects:

- a selected incremental snapshot whose full base is absent from the same file;
- deduplicated, redacted, resumed, or big-endian send streams;
- `apply` inputs containing more than one snapshot substream; and
- an update that deletes and recreates the target under a new DMU object.

## Validation and fixtures

The committed send fixtures were produced by OpenZFS 2.3.2 and cover plaintext full and incremental streams, a three-snapshot archive, AES-256-GCM raw encrypted sends, raw incremental and spill records, Zstandard raw blocks, compressed replay records, and embedded-data records. A separately licensed OpenZFS 2.2.2 fixture adds a real AES-256-GCM passphrase-encrypted pool image. Automated tests exercise stream inspection, snapshot selection, directory listing, extraction, missing/wrong-key rejection, authentication failure, native-pool objset/dnode/block authentication, plaintext and raw incremental updates, positioned block reads, codec handling, and checksum-corruption rejection. Native-pool fixture provenance and hashes are recorded in [`tests/fixtures/native-encrypted-pool.README.md`](tests/fixtures/native-encrypted-pool.README.md).

Inception mode additionally has a 63-case matrix spanning FAT12/16/32, exFAT, NTFS, ext4, and compatible ext2 across unpartitioned, MBR, and GPT layouts in raw, sparse QCOW2, and `monolithicSparse` VMDK containers. The real-volume fixture loader verifies compressed and raw SHA-256 values; focused tests cover sparse files, long and nested names, case-insensitive lookup, ext holes and symlinks, offsets, volume selection, corrupt partition metadata, and unsupported container profiles.

The larger Linux lab scenarios have validated:

- extraction and incremental update of a 20+ MiB file from a dataset containing other files;
- selection and reconstruction of three snapshot versions;
- authenticated extraction of a 20 MiB file from a raw encrypted send;
- authenticated directory listing and byte-exact extraction from a real native-encrypted pool dataset;
- authenticated application and chain reconstruction of a Zstandard-compressed raw incremental containing an SA spill block;
- extraction and incremental update from `zfs send -c -e`, including ordinary Zstandard writes and embedded data;
- direct extraction from a 6 GiB exported file vdev;
- matching LZ4 extraction from either leaf of a two-way mirror;
- Zstandard extraction from both embedded and ordinary DVA-backed blocks; and
- a release executable with no ZFS or system Zstandard runtime dependency.

See [`docs/test-evidence.md`](docs/test-evidence.md) for hashes and recorded lab results. Reproduction helpers are available in `scripts/`:

```sh
scripts/verify-fixtures.sh target/release/zfs-send-extract .artifacts/lab

scripts/verify-snapshot-selection.sh \
  target/release/zfs-send-extract \
  labpool/zfs-send-snapshots \
  .artifacts/snapshot-selection

scripts/verify-encrypted-send.sh \
  target/release/zfs-send-extract \
  labpool/zfs-send-encrypted \
  .artifacts/encrypted-verification

scripts/verify-advanced-streams.sh \
  target/release/zfs-send-extract \
  labpool \
  .artifacts/advanced-streams

scripts/verify-pool-member.sh \
  target/release/zfs-send-extract \
  /path/to/exported-member.vdev \
  tank/data@s1 \
  /payload/large.bin \
  expected-s1-sha256 \
  .artifacts/pool-member
```

For a native-encrypted pool snapshot, pass its key file as the final
`verify-pool-member.sh` argument after the output directory.

## Implementation notes

The replay-record parser follows OpenZFS's public [`dmu_replay_record_t`](https://github.com/openzfs/zfs/blob/master/include/sys/zfs_ioctl.h) wire format and Fletcher-4 stream semantics. Raw key unwrapping and block authentication follow OpenZFS [`zio_crypt.c`](https://github.com/openzfs/zfs/blob/master/module/os/linux/zfs/zio_crypt.c) and [`dsl_crypt.c`](https://github.com/openzfs/zfs/blob/master/module/zfs/dsl_crypt.c). ZPL ZAP and System Attribute decoding is provided by the Apache-2.0 [`zfs-forensic-core`](https://github.com/SecurityRonin/zfs-forensic) crate.

See [`docs/format-notes.md`](docs/format-notes.md) for the implementation's format mapping.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
