# ZFS send format notes

The parser is intentionally narrower than a complete `zfs receive` implementation.

Each replay record has a 312-byte `dmu_replay_record_t` header. The first eight bytes are the record type and the BEGIN-only payload length; a 304-byte tagged union follows. Type-specific payload sizes come from the OBJECT bonus length, WRITE logical/compressed length, SPILL length, or embedded physical length.

For little-endian substreams, the records needed by this project are:

| Record | Fields used | Purpose |
| --- | --- | --- |
| BEGIN | feature flags, to/from GUID, dataset name | profile and chain validation |
| OBJECT | object/type/bonus type, bonus bytes | ZPL metadata and logical size |
| WRITE | object, offset, logical size, payload | directory ZAPs and file data |
| FREE | object, offset, length | incremental hole/truncation replay |
| FREEOBJECTS | first object/count | detect deletion or object reuse |
| END | cumulative Fletcher-4 | integrity and completeness |

## Raw encrypted sends

A `zfs send -w` substream sets `DMU_BACKUP_FEATURE_RAW`. Its BEGIN payload contains a packed `crypt_keydata` nvlist. Depending on the originating OpenZFS version this nvlist can use native little-endian or XDR encoding; both are decoded. The keyformat, PBKDF2 parameters, cipher suite, wrapped master/HMAC keys, wrapping IV, and wrapping tag are taken from that nvlist.

Passphrases use PBKDF2-HMAC-SHA1 with the stream's little-endian 64-bit salt and iteration count. The resulting 256-bit wrapping key unwraps the dataset master key and 512-bit HMAC key using the declared AES-CCM/GCM suite. Version 1 authenticates the key GUID, cipher-suite number, and key version as little-endian AAD. A bad key therefore fails before any filesystem metadata is trusted.

Each encrypted WRITE carries an 8-byte salt, 12-byte IV, and 16-byte tag. HKDF-SHA512 derives its AES block key from the dataset master key and block salt. Object types OpenZFS marks as authenticated rather than encrypted remain plaintext but carry a truncated HMAC-SHA512, which is also checked. The resulting bytes are then decompressed according to the raw on-disk compression field; off and the OpenZFS LZ4 wrapper are currently implemented.

OBJECT bonuses are encrypted together at dnode-block granularity and protected by the preceding OBJECT_RANGE tag. Authentication requires more than concatenating bonus ciphertext. The implementation reconstructs each portable 64-byte dnode core, root block-pointer property/MAC tuple, holes, and indirect checksum-of-MAC tree, then supplies those bytes as AES AAD. Only after the range tag verifies are SA bonuses used for file size and path metadata.

Non-BEGIN record headers carry a cumulative Fletcher-4 checksum in their final 32 bytes. END also carries the checksum of the preceding substream in its type-specific body. The parser validates both and refuses truncated streams.

OpenZFS uses a `DMU_COMPOUNDSTREAM` BEGIN/END prelude for `zfs send -I`, replication, and property-bearing sends. Complete `DMU_SUBSTREAM` BEGIN/END pairs follow, and a zero-filled END record concludes the package. The reader recognizes those boundaries as well as concatenated packages. Snapshot selection follows `toguid -> fromguid` links backward in file order until it reaches a full substream (`fromguid == 0`); records from other datasets or branches are ignored during reconstruction.

The starting snapshot named to `zfs send -I` is not itself present in the compound stream. Extraction therefore requires either a full substream earlier in the same file or a selected snapshot that is already a full send. The CLI reports the missing base GUID instead of producing a partial file.

Path lookup starts at ZPL object 1 (the master node), reads its `ROOT` entry, then walks directory ZAP values. Directory values use the low 48 bits for the object number and the high nibble for the dirent type. Modern file sizes are decoded from the `DMU_OT_SA` bonus using the dataset's `SA_ATTRS -> REGISTRY/LAYOUTS` objects. The exact byte offset of `ZPL_SIZE` is saved in the extraction sidecar so a later incremental OBJECT record can update the output length without needing the full base stream again.

Primary references:

- [OpenZFS send replay structures](https://github.com/openzfs/zfs/blob/master/include/sys/zfs_ioctl.h)
- [OpenZFS receive/checksum logic](https://github.com/openzfs/zfs/blob/master/module/zfs/dmu_recv.c)
- [OpenZFS raw encryption logic](https://github.com/openzfs/zfs/blob/master/module/os/linux/zfs/zio_crypt.c)
- [OpenZFS wrapping-key metadata](https://github.com/openzfs/zfs/blob/master/module/zfs/dsl_crypt.c)
- [OpenZFS zstream framing](https://github.com/openzfs/zfs/blob/master/cmd/zstream/zstream_io.c)
- [zfs-forensic-core ZAP/SA/ZPL parser](https://github.com/SecurityRonin/zfs-forensic/tree/main/core/src)
