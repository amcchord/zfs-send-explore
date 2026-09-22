# Local ntfs 0.4.0 patch

Source: https://crates.io/crates/ntfs/0.4.0 (MIT OR Apache-2.0).
Only API change: `NtfsAttribute::record_data()` exposes the already validated,
sector-fixed attribute record. This lets the application inspect compression
unit, VCN and initialized-length metadata without rereading unfixed disk bytes.
Compression decoding remains in the shared application layer.
Upstream examples/testdata are omitted; the application retains its independent
filesystem matrix fixtures and tests. The example manifest entry was removed.
Text line endings and trailing whitespace are normalized for this repository.
