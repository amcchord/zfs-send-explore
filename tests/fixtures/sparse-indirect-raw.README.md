# Sparse encrypted indirect-block regression

Generated 2026-09-22 with OpenZFS on a disposable 128 MiB file vdev in AustinLand.
This synthetic fixture contains no real backup data or private keys.

- Dataset: `zfse_sparse_fixture/data@fixture`
- Encryption: AES-256-GCM, passphrase `public-sparse-regression-passphrase`
- Compression: LZ4, recordsize 128 KiB
- `sparse.bin`: 512 MiB, first bytes `first authenticated extent\n`, last 32 bytes
  `last authenticated extent\n` padded to 32 bytes with `!`, zero-filled elsewhere
- `hello.txt`: `sparse indirect block regression\n`
- Stream produced by `zfs send -w zfse_sparse_fixture/data@fixture`
- Stream SHA-256: `33c2dce9a0edf3fcc863bda68160a6b4c31c7408dc04976717cac7d5f7262aa4`
- Sparse file SHA-256: `360d1f0783c34aff93541608e5b97cd0fa1c42dea02d5b9e692b303c0f1f3ed8`

The entirely empty indirect subtrees must authenticate as hole pointers rather
than synthesized allocated indirect blocks. Before the fix, even listing the
root fails authentication. The test verifies the large sparse object's metadata
and extracts the independently hashed sibling without allocating 512 MiB in CI.
