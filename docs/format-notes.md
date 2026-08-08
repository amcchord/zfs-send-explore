# Format and architecture notes

The readers are intentionally narrower than a complete `zfs receive` implementation or general-purpose pool importer. This document records the supported wire and on-disk profiles, the integrity checks applied before data is trusted, and the reconstruction state needed for targeted extraction.

## Send-stream framing

Each replay record has a 312-byte `dmu_replay_record_t` header. The first eight bytes are the record type and the BEGIN-only payload length; a 304-byte tagged union follows. Type-specific payload sizes come from the OBJECT bonus length, WRITE logical/compressed length, SPILL length, or embedded physical length.

For little-endian substreams, the records needed by this project are:

| Record | Fields used | Purpose |
| --- | --- | --- |
| BEGIN | feature flags, to/from GUID, dataset name | profile and chain validation |
| OBJECT | object/type/bonus type, bonus bytes | ZPL metadata and logical size |
| WRITE | object, offset, logical/compressed size, compression, payload | directory ZAPs and file data |
| FREE | object, offset, length | incremental hole/truncation replay |
| FREEOBJECTS | first object/count | detect deletion or object reuse |
| SPILL | object, logical/compressed size, crypto fields | overflow SA metadata and raw spill-pointer authentication |
| WRITE_EMBEDDED | object, offset, block length, logical/physical size | embedded-data replay from `zfs send -e` |
| OBJECT_RANGE | first object/count, salt, IV, tag | raw dnode-block authentication |
| END | cumulative Fletcher-4 | integrity and completeness |

## Compressed and embedded replay records

For a non-raw `zfs send -c` stream, a nonzero `drr_compressiontype` means the WRITE payload length comes from `drr_compressed_size`. The decoder uses the record's OpenZFS codec identifier and requires the decompressed result to equal `drr_logical_size`. A zero compression field retains the original send behavior: the wire payload must already equal the logical size.

`zfs send -e` can replace small ordinary writes with `DRR_WRITE_EMBEDDED`. Its `drr_psize` bytes are rounded up to eight bytes on the wire, but the padding is not part of the compressed input. The payload expands to `drr_lsize`; replacing the logical `drr_length` block also zeroes bytes beyond that embedded value so an incremental replay cannot retain stale data from the prior block. Only OpenZFS embedded data type 0 is accepted. OpenZFS explicitly disallows WRITE_EMBEDDED in raw streams.

## Raw encrypted sends

A `zfs send -w` substream sets `DMU_BACKUP_FEATURE_RAW`. Its BEGIN payload contains a packed `crypt_keydata` nvlist. Depending on the originating OpenZFS version this nvlist can use native little-endian or XDR encoding; both are decoded. The keyformat, PBKDF2 parameters, cipher suite, wrapped master/HMAC keys, wrapping IV, and wrapping tag are taken from that nvlist.

Passphrases use PBKDF2-HMAC-SHA1 with the stream's little-endian 64-bit salt and iteration count. The resulting 256-bit wrapping key unwraps the dataset master key and 512-bit HMAC key using the declared AES-CCM/GCM suite. Version 1 authenticates the key GUID, cipher-suite number, and key version as little-endian AAD. A bad key therefore fails before any filesystem metadata is trusted.

Each encrypted WRITE carries an 8-byte salt, 12-byte IV, and 16-byte tag. HKDF-SHA512 derives its AES block key from the dataset master key and block salt. Object types OpenZFS marks as authenticated rather than encrypted remain plaintext but carry a truncated HMAC-SHA512, which is also checked. The resulting bytes are then decompressed according to the raw on-disk compression field; off, LZJB, LZ4, gzip levels 1-9, ZLE, and Zstandard share the same decoder as direct pool-member reads.

OBJECT bonuses are encrypted together at dnode-block granularity and protected by the preceding OBJECT_RANGE tag. Authentication requires more than concatenating bonus ciphertext. The implementation reconstructs each portable 64-byte dnode core, root block-pointer property/MAC tuple, holes, and indirect checksum-of-MAC tree, then supplies those bytes as AES AAD. Only after the range tag verifies are SA bonuses used for file size and path metadata.

When `DRR_OBJECT_SPILL` is set, the spill block pointer's portable property/MAC tuple follows the root pointers in that AAD. A matching raw SPILL record supplies the pointer's salt, IV, MAC, compression, logical/physical sizes, and protected payload. The block is authenticated, decrypted as `DMU_OT_SA`, decompressed, and decoded through its independent SA header/layout. This supports both modified spills and the unmodified spill records OpenZFS includes for receive compatibility.

Raw incremental streams update only changed leaf block pointers while the OBJECT_RANGE tag authenticates the resulting complete dnode block. A self-contained history reconstruction therefore carries prior leaf property/MAC state forward, replaces WRITE pointers, removes FREE ranges, rebuilds indirect MACs, and verifies each new range tag. A raw extraction sidecar stores the target 32-slot range's portable dnodes and leaf MAC state (never the dataset key), allowing a standalone raw incremental to perform the same authenticated transition during `apply`.

Non-BEGIN record headers carry a cumulative Fletcher-4 checksum in their final 32 bytes. END also carries the checksum of the preceding substream in its type-specific body. The parser validates both and refuses truncated streams.

OpenZFS uses a `DMU_COMPOUNDSTREAM` BEGIN/END prelude for `zfs send -I`, replication, and property-bearing sends. Complete `DMU_SUBSTREAM` BEGIN/END pairs follow, and a zero-filled END record concludes the package. The reader recognizes those boundaries as well as concatenated packages. Snapshot selection follows `toguid -> fromguid` links backward in file order until it reaches a full substream (`fromguid == 0`); records from other datasets or branches are ignored during reconstruction.

The starting snapshot named to `zfs send -I` is not itself present in the compound stream. Extraction therefore requires either a full substream earlier in the same file or a selected snapshot that is already a full send. The CLI reports the missing base GUID instead of producing a partial file.

Path lookup starts at ZPL object 1 (the master node), reads its `ROOT` entry, then walks directory ZAP values. Directory values use the low 48 bits for the object number and the high nibble for the dirent type. Modern file sizes are decoded from the `DMU_OT_SA` bonus or spill using the dataset's `SA_ATTRS -> REGISTRY/LAYOUTS` objects. The exact byte offset and storage location of `ZPL_SIZE` are saved in the extraction sidecar so a later incremental OBJECT or SPILL record can update the output length without needing the full base stream again.

## Backup-media containers

Pool sources may be an exact ZFS member, a GPT/MBR whole disk with one supported
member, or a ZFS member inside a LUKS partition. LUKS1/LUKS2 parsing, key-slot
unlock, and sector decryption run in-process through the pure-Rust `luks-core`
reader. The app exposes only positioned plaintext reads from the unlocked
payload; it does not mount the volume, install a driver, invoke `cryptsetup`, or
write to the container. A supplied container passphrase is rejected if the
selected source is not LUKS, which catches accidental key/source mismatches.

A pool named `slide` is reported as a Slide Box. Slide's downloaded raw dataset
key may be represented as 64 hexadecimal characters even when OpenZFS metadata
declares `keyformat=raw`; that representation is decoded in memory to the
required 32 bytes before the existing wrapped-key authentication runs.

A LUKS-backed pool whose name starts with `revRT-` is reported as Datto Reverse
RoundTrip. Ordinary `.datto` images use the normal inception path. For a
`.detto` image, the reader locates the sole `/config/*.encryptionKeyStash` file
unless an explicit path is supplied, bounds its read, and authenticates the
first compact `user_keys_jwe` entry. The agent password derives an AES-256-GCM
key with PBKDF2-HMAC-SHA3-256 using the protected `p2s` and bounded `p2c`
parameters; the compact protected header is authenticated as JWE additional
data. The recovered 64-byte key supplies the two AES-256 keys for XTS
decryption. Positioned reads expand to complete 512-byte sectors and use
`plain64` sector numbers before returning only the requested plaintext range.
Temporary key plaintext and application-owned key buffers are zeroized.

## Pool-member reads

Before vdev parsing, a whole-disk source gets one conservative partition-discovery pass if it has no labels at disk-relative offsets. The reader recognizes primary GPT headers at LBA 1 for 512-byte and 4096-byte logical sectors, validates the header and complete partition-array CRC-32 values, caps the table at 64 MiB, bounds each LBA conversion against the source, and also bounds primary MBR partition entries. Each non-empty partition becomes an offset read-only view. Plain candidates are probed for ZFS labels; LUKS candidates are unlocked first when a container key is available. It proceeds only when exactly one view yields a supported ZFS member.

The direct backend starts at each of the four 256 KiB vdev labels and uses the highest valid uberblock transaction group found there. A DVA is translated to a member-relative byte offset as `(offset << 9) + 4 MiB`; this release accepts only top-level vdev id 0 and rejects any pool whose label reports more than one top-level vdev. Mirror replication happens below that top-level address, so the same offset can be read from either healthy leaf of a one-mirror pool.

Blocks are fetched with positioned reads and never cached as a whole-vdev buffer. Every Fletcher-2, Fletcher-4, or SHA-256 checksum present is checked before decompression; a mismatch is fatal for that DVA, and alternate DVA copies are tried before extraction fails. A block explicitly configured with checksum `off` has no checksum to validate. Inherit/default sentinels, unsupported checksum algorithms, gang blocks, and DVAs naming unavailable top-level vdevs fail explicitly.

### Native-encrypted pool datasets

The selected DSL dataset's `ds_bp` crypt bit determines whether a key is
required. Key discovery starts at that dataset's DSL directory and follows
`dd_parent_obj` until it finds `com.datto:crypto_key_obj`, matching OpenZFS key
inheritance. The referenced fat ZAP supplies the cipher suite, crypto GUID and
version, wrapped master/HMAC keys, wrapping IV/MAC, key format, and PBKDF2
parameters. Integer ZAP arrays are converted from their on-disk big-endian
element representation before the same bounded raw/hex/passphrase unwrap path
used by encrypted sends is invoked.

Native-encryption overloads block-pointer crypt bit 61 for three cases. An
encrypted level-zero type carries ciphertext; an authenticated level-zero type
keeps plaintext and carries a truncated HMAC-SHA512; and an indirect pointer
carries a SHA-512 checksum of its children's portable property/MAC tuples.
Before any of those checks, the ordinary physical checksum is verified. For
Fletcher-2/4 protected pointers, OpenZFS XOR-folds checksum words 2/3 into words
0/1 before replacing the discarded half with the MAC, so the reader reproduces
that fold rather than comparing an unmodified checksum.

The objset block is plaintext and has a special portable HMAC over its type,
portable flags, and normalized meta-dnode. Dnode-array blocks are partially
encrypted: each portable 64-byte core, normalized block-pointer tuple, spill
pointer, and unencrypted bonus region becomes AEAD additional data, while
encrypted bonus regions are gathered and decrypted as one stream. Directory
ZAPs, SA data, and ordinary file blocks use whole-block AES-CCM/GCM after
HKDF-SHA512 key derivation. Authentication always precedes decompression or ZPL
parsing, and every failed DVA can fall back to another recorded copy.

The implemented suites are AES-128/192/256-CCM and AES-128/192/256-GCM with
crypto-key format versions 0 and 1. Native-encrypted big-endian pool datasets
remain explicitly unsupported until byte-swapped objset, dnode, BP-MAC, and
AEAD parameter vectors are available. Plaintext big-endian pool support is
unchanged.

OpenZFS Zstandard blocks start with a big-endian compressed length and version/level word, followed by a Zstandard frame written without the standard four-byte magic. The reader bounds the input using that compressed length, restores the standard magic for the pure-Rust decoder, and requires the result to match the block pointer's logical size exactly. This works for both ordinary DVA-backed blocks and compressed payloads in embedded block pointers.

The MOS object directory's `root_dataset` entry opens the DSL directory tree. `dd_child_dir_zapobj` enumerates filesystem datasets, while the head dataset's `ds_snapnames_zapobj` maps snapshot names to snapshot dataset objects. Each chosen head or snapshot contributes its `ds_bp`, which roots a ZPL objset. Named snapshots use the permanent `ds_guid` at bonus offset 112 for send-compatible extraction sidecars.

Embedded block pointers store 14 payload words inline, excluding `blk_prop` (word 6) and logical birth (word 10). Payload bytes are reconstructed from each decoded word's low bits first, independent of pool byte order, before the normal compression decoder runs. This matters for freshly created pools, where small MOS and ZAP blocks are commonly embedded and LZ4-compressed.

## Inception-mode layered reads

Inception mode converts one resolved regular ZPL file into the same finite positioned-read interface for both source backends. A pool source translates each requested range into dnode block ids, follows direct or indirect block pointers, authenticates/decrypts a native-encrypted outer dataset when selected, verifies and decompresses only those blocks, and fills holes or short final blocks with zeroes. A send source scans the selected full/incremental chain once and builds a non-overlapping interval map whose leaves identify replay payload offsets and encodings. Later reads reopen payloads with positioned I/O, confirm the payload CRC captured during the checked scan, authenticate/decrypt raw records where necessary, decode only intersecting blocks, and retain one decoded block in a synchronized cache.

An explicit image offset and length first create a hard-bounded view of the ZFS file. Container detection then recognizes QCOW2 by `QFI\xfb` and VMDK sparse extents by `KDMV`; an unrecognized prefix remains raw. QCOW2 v2/v3 cluster lookup handles unallocated/zero, ordinary, and DEFLATE-compressed clusters, but rejects backing files, encryption, and incompatible external/extended layouts. The VMDK reader accepts only an embedded-descriptor `monolithicSparse` extent and rejects descriptor-only, split, flat, and stream-optimized layouts rather than attempting to locate other files.

Partition discovery tries GPT before MBR. GPT candidates at primary and final LBAs are considered with 512-byte and 4096-byte logical sectors. Header CRC-32, current/alternate LBAs, usable bounds, entry count/size, a 64 MiB entry-array cap, entry-array CRC-32, and every non-empty partition range are validated. A damaged primary can therefore fall back to a valid backup. MBR discovery bounds primary partitions and follows EBR chains with an explicit extended-container bound, loop detection, partition-type validation, and a 128-entry cap. If neither table yields a partition, the entire virtual disk is probed as a superfloppy filesystem.

Filesystem probes are signature-gated and then require the corresponding parser to open successfully. FAT BPB invariants are used instead of trusting the informational `FAT12`/`FAT16`/`FAT32` label. Directory resolution normalizes absolute paths and refuses `..` or NUL components. NTFS lookup uses the volume `$UpCase` table; extraction accepts the unnamed `$DATA` stream but refuses NTFS compression and EFS. FAT12/16/32 and exFAT use cluster-chain-bounded reads. ext4 and compatible ext2 lookup uses byte paths, exposes regular/directory/symlink types, refuses names the UTF-16 Windows UI cannot represent losslessly, and does not follow symlinks for extraction.

The destination path is the only write target in this stack. File data passes through sparse extent writes and SHA-256 into a temporary file in the destination directory; length and byte count must match before synchronization and atomic persistence. Recursive extraction builds the entire requested tree in a sibling temporary directory, never follows symlinks or special entries, and publishes the directory only after every regular file succeeds. With forced replacement, the previous destination is staged for rollback until the new tree is renamed into place. No container, partition, filesystem, ZFS stream, or pool-member write API is exposed.

Primary references:

- [OpenZFS send replay structures](https://github.com/openzfs/zfs/blob/master/include/sys/zfs_ioctl.h)
- [OpenZFS receive/checksum logic](https://github.com/openzfs/zfs/blob/master/module/zfs/dmu_recv.c)
- [OpenZFS raw encryption logic](https://github.com/openzfs/zfs/blob/master/module/os/linux/zfs/zio_crypt.c)
- [OpenZFS wrapping-key metadata](https://github.com/openzfs/zfs/blob/master/module/zfs/dsl_crypt.c)
- [OpenZFS zstream framing](https://github.com/openzfs/zfs/blob/master/cmd/zstream/zstream_io.c)
- [OpenZFS block-pointer and DVA layout](https://github.com/openzfs/zfs/blob/master/include/sys/spa.h)
- [OpenZFS embedded block-pointer codec](https://github.com/openzfs/zfs/blob/master/module/zfs/blkptr.c)
- [OpenZFS DSL dataset and directory layouts](https://github.com/openzfs/zfs/blob/master/include/sys/dsl_dataset.h)
- [OpenZFS Zstandard block codec](https://github.com/openzfs/zfs/blob/master/module/zstd/zfs_zstd.c)
- [OpenZFS Zstandard header](https://github.com/openzfs/zfs/blob/master/include/sys/zstd/zstd.h)
- [zfs-forensic-core ZAP/SA/ZPL parser](https://github.com/SecurityRonin/zfs-forensic/tree/main/core/src)
- [Slide Box manual-access guide](https://docs.slide.tech/guides/manually-accessing-slide-box-backups/)
- [Datto Reverse RoundTrip manual-access guide](https://docs.slide.tech/guides/manually-accessing-datto-reverse-roundtrip-backups/)
- [`luks-core` read-only LUKS library](https://docs.rs/luks-core/latest/luks/)
