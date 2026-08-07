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
