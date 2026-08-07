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

## Pool-member reads

Before vdev parsing, a whole-disk source gets one conservative partition-discovery pass if it has no labels at disk-relative offsets. The reader recognizes primary GPT headers at LBA 1 for 512-byte and 4096-byte logical sectors, validates the header and complete partition-array CRC-32 values, caps the table at 64 MiB, bounds each LBA conversion against the source, and probes non-empty partitions as offset read-only views. It proceeds only when exactly one view has ZFS vdev labels.

The direct backend starts at each of the four 256 KiB vdev labels and uses the highest valid uberblock transaction group found there. A DVA is translated to a member-relative byte offset as `(offset << 9) + 4 MiB`; this release accepts only top-level vdev id 0 and rejects any pool whose label reports more than one top-level vdev. Mirror replication happens below that top-level address, so the same offset can be read from either healthy leaf of a one-mirror pool.

Blocks are fetched with positioned reads and never cached as a whole-vdev buffer. Every Fletcher-2, Fletcher-4, or SHA-256 checksum present is checked before decompression; a mismatch is fatal for that DVA, and alternate DVA copies are tried before extraction fails. A block explicitly configured with checksum `off` has no checksum to validate. Inherit/default sentinels, unsupported checksum algorithms, gang blocks, and DVAs naming unavailable top-level vdevs fail explicitly.

OpenZFS Zstandard blocks start with a big-endian compressed length and version/level word, followed by a Zstandard frame written without the standard four-byte magic. The reader bounds the input using that compressed length, restores the standard magic for the pure-Rust decoder, and requires the result to match the block pointer's logical size exactly. This works for both ordinary DVA-backed blocks and compressed payloads in embedded block pointers.

The MOS object directory's `root_dataset` entry opens the DSL directory tree. `dd_child_dir_zapobj` enumerates filesystem datasets, while the head dataset's `ds_snapnames_zapobj` maps snapshot names to snapshot dataset objects. Each chosen head or snapshot contributes its `ds_bp`, which roots a ZPL objset. Named snapshots use the permanent `ds_guid` at bonus offset 112 for send-compatible extraction sidecars.

Embedded block pointers store 14 payload words inline, excluding `blk_prop` (word 6) and logical birth (word 10). Payload bytes are reconstructed from each decoded word's low bits first, independent of pool byte order, before the normal compression decoder runs. This matters for freshly created pools, where small MOS and ZAP blocks are commonly embedded and LZ4-compressed.

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
