# Initial milestone evidence

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
