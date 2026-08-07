# ZFS send format notes

The initial parser is intentionally narrower than a complete `zfs receive` implementation.

Each replay record has a 312-byte `dmu_replay_record_t` header. The first eight bytes are the record type and the BEGIN-only payload length; a 304-byte tagged union follows. Type-specific payload sizes come from the OBJECT bonus length, WRITE logical/compressed length, SPILL length, or embedded physical length.

For plain little-endian substreams, the records needed by this project are:

| Record | Fields used | Purpose |
| --- | --- | --- |
| BEGIN | feature flags, to/from GUID, dataset name | profile and chain validation |
| OBJECT | object/type/bonus type, bonus bytes | ZPL metadata and logical size |
| WRITE | object, offset, logical size, payload | directory ZAPs and file data |
| FREE | object, offset, length | incremental hole/truncation replay |
| FREEOBJECTS | first object/count | detect deletion or object reuse |
| END | cumulative Fletcher-4 | integrity and completeness |

Non-BEGIN record headers carry a cumulative Fletcher-4 checksum in their final 32 bytes. END also carries the checksum of the preceding substream in its type-specific body. The parser validates both and refuses truncated streams.

Path lookup starts at ZPL object 1 (the master node), reads its `ROOT` entry, then walks directory ZAP values. Directory values use the low 48 bits for the object number and the high nibble for the dirent type. Modern file sizes are decoded from the `DMU_OT_SA` bonus using the dataset's `SA_ATTRS -> REGISTRY/LAYOUTS` objects. The exact byte offset of `ZPL_SIZE` is saved in the extraction sidecar so a later incremental OBJECT record can update the output length without needing the full base stream again.

Primary references:

- [OpenZFS send replay structures](https://github.com/openzfs/zfs/blob/master/include/sys/zfs_ioctl.h)
- [OpenZFS receive/checksum logic](https://github.com/openzfs/zfs/blob/master/module/zfs/dmu_recv.c)
- [OpenZFS zstream framing](https://github.com/openzfs/zfs/blob/master/cmd/zstream/zstream_io.c)
- [zfs-forensic-core ZAP/SA/ZPL parser](https://github.com/SecurityRonin/zfs-forensic/tree/main/core/src)

