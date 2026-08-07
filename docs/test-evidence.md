# Validation evidence

The committed integration fixtures and the larger lab scenarios below were produced with OpenZFS 2.3.2 on Debian 13 x86-64. Fixture tests run without ZFS; the lab runs independently compare extracted files with mounted source snapshots and record exact sizes and SHA-256 hashes.

## Native Windows client validation

The x86-64 Windows GUI was exercised on Microsoft Windows 11 Pro build 26100 using the real native controls and file dialogs documented in [`windows-client.md`](windows-client.md). The final tested executable had SHA-256 `d5ced64ce03b607352309a1d6ca3443515dd2f7494ddc3de211451080fa21a9e`.

The UI run opened `multi-snapshot.zfs`, selected all three full/incremental views, and extracted a file from `s2` with its update sidecar. A second base extraction from `tiny-full.zfs` was advanced with `tiny-incremental.zfs`; the target changed from 29 to 58 bytes and matched the expected two-line incremental content. The raw `encrypted-raw-s1.zfs` fixture was opened with its passphrase key, authenticated, and browsed through `/docs/hello.txt`.

For direct-drive coverage, an isolated 1 GiB single-file vdev was exported and attached to Windows read-only as `\\.\PhysicalDrive1`. The client enumerated `archive@baseline`, `archive@after-update`, and both current read-only dataset heads without importing or mounting the pool. It then extracted a 268,435,456-byte sparse file whose first and last markers matched the source. NTFS reported `Archive, SparseFile` and only two 131,072-byte allocated ranges:

| Range | Offset | Length |
| --- | ---: | ---: |
| First marker | `0x0` | `0x20000` |
| Last marker | `0xffe0000` | `0x20000` |

The run exposed and verified fixes for three Windows/on-disk edge cases: releasing the open base-file handle before atomic replacement, using `IOCTL_DISK_GET_LENGTH_INFO` when `File::metadata()` rejects a raw drive, and zero-extending short or ancestor-hole ZFS leaves to the dnode record size during sparse pool extraction. The full unit/integration suite, native clippy, Windows-target clippy, and release cross-build passed after those changes.

## Inception-mode validation

The compositional inception matrix runs 63 end-to-end cases without a host mount or filesystem driver: seven filesystem variants (FAT12, FAT16, FAT32, exFAT, NTFS, ext4, and compatible ext2), each as an unpartitioned filesystem, an MBR partition, and a GPT partition, then each resulting virtual disk as raw, sparse QCOW2 v3, and VMDK `monolithicSparse`. Every case probes the container and volume, lists the real root directory, resolves a nested path, extracts a regular file through the selected volume, and compares exact content. The sparse-container builders map every nonzero guest cluster or grain, so filesystem data and metadata can live anywhere in the virtual disk rather than only in its first allocation unit.

The committed real-volume corpus is base64-wrapped Zstandard and occupies about 1.3 MiB in source form. Its loader verifies both compressed and decompressed SHA-256 before a parser sees the bytes. Exact sources, commits, licenses, sizes, hashes, and the locally generated FAT32 contents are recorded in [`../tests/fixtures/inception/README.md`](../tests/fixtures/inception/README.md). Focused oracles cover resident, non-resident, and sparse NTFS data; a 512-entry NTFS directory index; FAT/exFAT case-insensitive lookup, nested paths, and long filenames; FAT32 zero-filled data; ext2/ext4 holes; and refusal to follow ext symlinks during extraction.

Additional tests cover explicit offset-and-length windows around all three container types, multiple-volume selection, primary and logical MBR partitions, backup-GPT recovery, both-GPT-copy corruption, looping EBRs, out-of-bounds partitions, QCOW1, QCOW2 backing/encryption rejection, external VMDK descriptors, traversal/relative-path rejection, unknown-filesystem diagnostics, and invalid image windows. CLI parser tests exercise send-stream and pool-member `inspect`, `list`, and `extract` forms with volume selectors and byte windows. A UI-service test retains an inspected session, lists it, extracts through it, and confirms nested extraction is not presented as incrementally updateable.

This matrix exposed and verified two production fixes: FAT/exFAT lookup now follows case-insensitive filesystem semantics, and a validated filesystem at byte zero now takes precedence over false MBR entries formed by arbitrary NTFS/exFAT boot-code bytes. GPT is still checked first because its checksummed metadata is unambiguous.

Separate tests drive the new positioned ZFS-file source against committed real OpenZFS streams. They read `/hello.txt` directly from `tiny-full.zfs`, reconstruct the longer `/version.txt` value at `s2` across the full/incremental chain in `multi-snapshot.zfs`, and authenticate/decrypt `/docs/hello.txt` from `encrypted-raw-s1.zfs`. These tests compare exact bytes while bypassing ordinary whole-file extraction, so the replay interval map, payload positioning, incremental overwrite semantics, raw key path, and block cache all participate.

## Initial send-stream milestone

The initial end-to-end test ran on a Debian 13 x86-64 VM with OpenZFS 2.3.2. ZFS was used to produce the streams; the release CLI was then built as a normal userspace executable and run against the files.

| Artifact/result | Value |
| --- | --- |
| Full stream | 21,079,184 bytes, 174 WRITE records |
| Incremental stream | 2,513,008 bytes, 22 WRITE records |
| Extracted base file | 20,971,520 bytes |
| Base SHA-256 | `6a9f57f42ef35cf53e75a87b36686aebc4ea309d5a7d45406b11a7c4a0bb98aa` |
| Updated file | 23,068,672 bytes |
| Updated SHA-256 | `ee9df4d14996075c1de7e02a26530b4aa38a3d5e0fa18b2af006d663710c3e54` |

The full dataset also contained nested directories, configuration files, and ordinary sibling files. The CLI resolved `/payload/large.bin` to DMU object 6, extracted it without receiving or mounting the stream, then applied three changed ranges and a 2 MiB append from the incremental stream. Both hashes matched the live source file at the corresponding snapshot.

`ldd` on the Linux release executable reported only glibc, `libgcc_s`, and the ELF loader. `readelf` found no ZFS dynamic dependency.

## Multi-snapshot selection

A second OpenZFS 2.3.2 fixture contains a full `s1` substream followed by the compound output of `zfs send -I ...@s1 ...@s3`. The 77,944-byte file contains three snapshot substreams plus the compound prelude and conclusion. Integration tests select snapshots by short name, full `dataset@snapshot` name, and GUID, then verify these versions of `/version.txt`:

| Snapshot | Expected content size |
| --- | ---: |
| `s1` | 13 bytes |
| `s2` | 32 bytes |
| `s3` | 6 bytes |

The test also confirms a file created at `s2` is visible there and absent after its deletion at `s3`, proving that directory metadata and `FREE` records are replayed across the selected GUID chain.

The reproducible large-file script was also run on the Debian/OpenZFS lab host. It produced a 22,538,592-byte history file (21,072,688-byte full base plus 1,465,904 bytes of compound incrementals) and matched the live snapshot hashes at all three points:

| Snapshot | Extracted size | SHA-256 |
| --- | ---: | --- |
| `s1` | 20,971,520 bytes | `15fa055a60f6da0341d892bff2a040bf3fb30e34d1c7ae19f731ee8ce857610c` |
| `s2` | 22,020,096 bytes | `becbc985864fef014a87dd918a19bf8a76525b5d9105c965053bf4c87de38f2e` |
| `s3` | 19,922,944 bytes | `6182da40e65e8850150581e9b9aba93b9286006933467122b207aaa74a62d0b8` |

Linux integration tests passed against the committed real-stream fixtures. The release executable continued to link only glibc, `libgcc_s`, and the ELF loader—not ZFS.

## Raw encrypted send

An OpenZFS 2.3.2 dataset using `encryption=aes-256-gcm`, `keyformat=passphrase`, and `compression=off` produced the committed 21,524-byte `encrypted-raw-s1.zfs` fixture with `zfs send -w`. The fixture uses the intentionally public test passphrase `zfs-send-fixture-passphrase` and PBKDF2 iteration count 100,000.

The automated test verifies wrapped-key authentication, HMAC verification for the ZPL master block, AES-GCM decryption of the file block, rejection of a modified block tag, authenticated dnode bonus reconstruction, directory lookup, CLI extraction, and rejection of an incorrect passphrase. `/docs/hello.txt` extracts to 16 bytes with SHA-256 `6e219bc096755477b1a30301446ac364f6291f16ce381d561947cdecd3fab265`.

A fresh run of `scripts/verify-encrypted-send.sh` on the Debian/OpenZFS lab created a 21,045,676-byte raw stream containing directories, metadata objects, a sibling file, and a deterministic 20,971,520-byte file. The stream contained 170 WRITE records. The release CLI reconstructed the file's indirect checksum-of-MAC tree, authenticated its dnode range, and extracted all 20 MiB with SHA-256 `e9289240e662525b05f84921ec0f23f737f52cbae3da9ff6ed5ffa3459e84d0d`, matching the live snapshot. `ldd` again showed only glibc, `libgcc_s`, and the ELF loader.

## Offline pool-member extraction

The same Debian 13/OpenZFS 2.3.2 Proxmox lab was used to export the existing `labpool` file vdev and point the release CLI directly at its 6,442,450,944-byte member. The reader selected active txg 588 and enumerated six filesystem datasets and nine named snapshots without loading or importing the pool.

From `labpool/zfs-send-snapshots@s1`, it listed `/payload`, extracted object 4, and produced the independently recorded snapshot result:

| Result | Value |
| --- | --- |
| Extracted size | 20,971,520 bytes |
| SHA-256 | `15fa055a60f6da0341d892bff2a040bf3fb30e34d1c7ae19f731ee8ce857610c` |
| Snapshot GUID | `0xe7d3232d902916ef` |

An ordinary OpenZFS `s1 -> s2` incremental send was then applied to that pool-extracted file. The updated file grew to 22,020,096 bytes, its SHA-256 became `becbc985864fef014a87dd918a19bf8a76525b5d9105c965053bf4c87de38f2e`, and its sidecar advanced to the exact `s2` GUID `0xad1db3de25c00357`.

Mirror behavior was checked with a separate pool containing one two-file mirror, each member 1 GiB. After export, each member was supplied to the CLI independently. Both extracted the same 22,020,096-byte LZ4-compressed snapshot file with SHA-256 `becbc985864fef014a87dd918a19bf8a76525b5d9105c965053bf4c87de38f2e`. This also exercised embedded LZ4 metadata blocks in the freshly created pool.

The mirror pool was then extended with a `compression=zstd-3` filesystem. Two independent Zstandard layouts were verified from each exported mirror leaf:

| Zstandard case | Size | SHA-256 | On-disk layout |
| --- | ---: | --- | --- |
| Highly compressible `large.bin` | 22,020,096 bytes | `becbc985864fef014a87dd918a19bf8a76525b5d9105c965053bf4c87de38f2e` | 128 KiB records compressed into embedded block pointers |
| Moderately compressible `moderate.bin` | 20,971,520 bytes | `f5e1ae7ccad186d875cfe5b5b2d7a50f3b45ad1228a866234cdd8c725edc8d26` | ordinary DVA-backed 128 KiB records with 66,048-byte physical blocks |

All four Zstandard extractions matched their source hashes. The codec tests additionally cover off, LZJB, LZ4, gzip, ZLE, Zstandard framing, malformed Zstandard lengths, and rejection of invalid on-disk inherit/default sentinels.

Guardrail checks used a two-top-level-vdev stripe, which was rejected from either incomplete member, and a native-encrypted dataset, which produced an explicit unsupported-profile error before any encrypted block was decoded. Current-head extraction was also checked independently: it returned the expected 19,922,944-byte file and removed a deliberately stale incremental-send sidecar.

## Advanced send profiles

OpenZFS 2.3.2 produced four committed fixtures for the advanced send paths. The raw dataset used `encryption=aes-256-gcm`, `compression=zstd-3`, a forced SA spill, and `zfs send -w`; the plaintext dataset used `compression=zstd-3`, `embedded_data=on`, and `zfs send -c -e`.

| Fixture | Size | Relevant replay records |
| --- | ---: | --- |
| `advanced-raw-full.zfs` | 38,116 bytes | 26 WRITE, 3 OBJECT_RANGE, 1 SPILL |
| `advanced-raw-incremental.zfs` | 9,524 bytes | 3 WRITE, 1 OBJECT_RANGE, 1 SPILL |
| `advanced-plain-full.zfs` | 1,108,784 bytes | 20 WRITE, 5 WRITE_EMBEDDED |
| `advanced-plain-incremental.zfs` | 137,336 bytes | 2 WRITE, 1 WRITE_EMBEDDED |

The raw full send extracted a 2,097,152-byte file with SHA-256 `9aa35c1088ccaa0785d8c10fa23c740e9202368d29fb2e4972ec10a28d9d490f`. Applying its standalone raw incremental authenticated the changed leaf block pointers, SA spill, and updated OBJECT_RANGE tag, producing 2,359,296 bytes with SHA-256 `db5d733aba08cebf9d52621e01f64fe925c071a74de697dc2ba4792bfac38d28`. Selecting the second snapshot from the concatenated raw chain produced the same hash. The test also confirmed that the fixture's protected WRITE records use OpenZFS compression type 16 (Zstandard), that the extraction sidecar contains no key, and that raw apply rejects a missing key.

For `zfs send -c -e`, the first snapshot's ordinary file extracted to 2,097,152 bytes with SHA-256 `af2e223b435354ed53190ee2e9cbbe8612e90ab691639aa91f71dce0de47a027`; its embedded-data file extracted to 4,096 bytes with SHA-256 `fcf23bb6294ddeca564cb0cf6a256dd15dc01516a792f644b694e172e4f7f89f`. Incremental application advanced them to SHA-256 `c8a198dbc1e37af9ae2899906d7b95266fc2c1e2e3f49f5a5b8c3463d14ef368` (2,228,224 bytes) and `e45b0cd2e205653ec280d92f9bad6b9b793f0e98c948dac72cd0f558de7ba1c5` (4,096 bytes), respectively.

`scripts/verify-advanced-streams.sh` recreated both datasets, streams, and source hashes in the Proxmox lab. Every extracted or updated result matched the corresponding mounted ZFS snapshot, and the complete Rust test suite exercises the committed fixtures without requiring ZFS.
