# Inception filesystem fixtures

These images are deterministic, compressed test inputs for the read-only inception-mode matrix. Tests decode the base64 text, decompress Zstandard in memory, and never mount or modify an image. Keeping the encoded form makes the fixtures portable through source archives and text-only patch tooling.

| Fixture | Raw bytes | Raw SHA-256 | Zstandard SHA-256 | Provenance |
| --- | ---: | --- | --- | --- |
| `ntfs.img.zst.b64` | 2,097,152 | `e3612c182b8010e3599b5eb93bff427c7d824e85bdc2ddbe46e378e3ba814eb9` | `d53e13e45543501d861898f70e70d26a731ad79d4245ac6039aab6235d555dad` | `ntfs` 0.4.0 `testdata/testfs1` |
| `fat16.img.zst.b64` | 4,194,304 | `b8dee10dcb38b6e6dfefe9e5a551405bdb1e600fb81789e80420070abdc71f8b` | `2f2ec24e7e62ae256eeb3197540e28822f130a4cf1130d812a2b6dba0062a8dd` | `fat-core` test image at commit `e35d2869fbf5c5539df497db79080c86cb68b0be` |
| `fat32.img.zst.b64` | 67,108,864 | `c43eced7ec3fe9dd78a9ee402d5ecb691f58688b377e7445aa4e8e08236a0045` | `ed871400721a9fa11f0839bb2bb51d2bde132eec9e10f40875b414d02a9c3e3b` | Locally generated as described below |
| `exfat.img.zst.b64` | 2,097,152 | `4e0ab00a9c753bc20f9ece484534bbcdab59c1688a1d30590426ce4b9dba0601` | `cdd8cbc4944cf92fab78b2d325ce03444527713906e0fab7c66e3a16c9cbf136` | `fat-core` test image at commit `e35d2869fbf5c5539df497db79080c86cb68b0be` |
| `ext4.img.zst.b64` | 67,108,864 | `58f4ec5f880a1934bc23a73bc0d07b50a987736d7a1a8fbbafdc99bd36dfbee3` | `17c740fa68d260e70a3e8e146c1537f8f5b2fee3eb3a65c6385cd493ecb5a09b` | `ext4-view` `test_data/test_disk1.bin.zst` at commit `0c704fa3eed5f9faa47940f1c1b29e60f066f463` |
| `ext2.img.zst.b64` | 100,663,296 | `b277b932c8f001c302920cbe9f47245b82e3232a123c82e5a595c5fd09c8dc3f` | `b7008bdc6a2d50fdcb0684e506632e39421e18db7dbdc4d56c12422656102511` | `ext4-view` `test_data/test_disk_ext2.bin.zst` at commit `0c704fa3eed5f9faa47940f1c1b29e60f066f463` |

The `ntfs` and `ext4-view` projects are licensed MIT OR Apache-2.0. The `fat-core` project and its test images are Apache-2.0. Their upstream content oracles are retained in the tests: resident, non-resident, and sparse NTFS files; FAT/exFAT long and nested names; and ext2/ext4 regular, sparse, and symlink cases.

The FAT32 image fills the upstream FAT32 coverage gap (the upstream repository intentionally omits its large raw image). It was formatted with macOS `newfs_msdos -F 32 -v ZFSEFAT32` and contains:

- `/HELLO.TXT`: `hello from FAT32 matrix\n`
- `/Long Matrix Filename FAT32.txt`: `long FAT32 filename content\n`
- `/subdir/NESTED.TXT`: `nested FAT32 matrix content\n`
- `/SPARSE.BIN`: `HEAD`, zero bytes through offset 4 MiB, then `TAIL`

All six raw images were recompressed with `zstd -19` before base64 encoding. The SHA-256 values above are asserted by the fixture-loading test, so accidental fixture drift is reported independently of parser behavior.
