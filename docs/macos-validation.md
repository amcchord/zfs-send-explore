# macOS and Windows restore qualification — 2026-09-22

## Tested implementation

Native macOS SwiftUI app with a bundled persistent Rust service using the existing
`client` library. The standalone CLI shares the same parser and extraction code.
Mac packaging produces an app ZIP and a separate CLI tarball; Apple Silicon and
Intel jobs are defined in CI and release workflows. Local execution was on Apple
Silicon only; Intel builds and tests are qualified separately by the hosted
release CI. Interactive real-Slide GUI testing was on Apple Silicon.

## Real Slide fixture

An existing AES-256-GCM/raw-key Linux backup snapshot in AustinLand was transferred
with `zfs send -w`, then received **without mounting** into a disposable 4 GiB
single-file pool and exported. The original striped Slide pool remained online.
The image contains the real encrypted dataset and a 40 GiB sparse GPT disk with
ext4 and FAT32 partitions. The private stream, pool image, and key are retained
only in the local project workspace, outside this repository.

An independent read-only loop device plus `debugfs` on the original snapshot
provided `/etc/hostname` as the oracle. The loop was detached after the read.

| Restore path | Result |
| --- | --- |
| Mac CLI, encrypted pool → raw disk → GPT1/ext4 → file | Pass |
| Mac GUI, select snapshot → key file → disk image → volume → file | Pass |
| Mac CLI, raw encrypted send → raw disk → GPT1/ext4 → file | Pass |
| Clean Windows 11 CLI, same encrypted pool and nested file | Pass |
| Windows 11 GUI, key file → nested GPT1/ext4 → file | Pass |

Each passed path restored 16 bytes with SHA-256
`b2b9ebacbf34320c03f2a3b48454ac82999b59bb816848ed69f385e738874307`,
matching the independent snapshot oracle. A 29-byte plain OpenZFS fixture also
passed manual Mac GUI restoration, with its independently recorded fixture hash.

## Defects found during qualification

- The first Mac prototype blocked waiting to fill a pipe read. The final client
  consumes available response data and keeps parsing until a newline arrives.
  Persistent-process integration tests enforce a response before stdin is closed.
- Raw encrypted sends containing completely sparse indirect subtrees failed dnode
  authentication. Reconstruction now retains those subtrees as hole pointers.
  The 23 KiB synthetic `sparse-indirect-raw.zfs` fixture fails on the previous
  code and passes with the fix; authentication remains mandatory.
- Published v0.5.1 Windows clients failed to launch on a clean machine with
  status `0xc0000135`, because `VCRUNTIME140.dll` was absent. MSVC builds now link
  the C runtime statically. Native rebuilt clients no longer import that DLL;
  the CLI restored the real Slide file on the same unmodified clean Windows OS.
- The Windows credentials button clipped its stored-key count. Its layout now
  reserves enough width for the label. Mac browsing also has an explicit Restore
  button, readable partition sizes, search, and a checksum confirmation.

## Checks and limits

- 64 Rust tests passed on Apple Silicon macOS and native x86-64 Windows.
- Rust formatting and clippy with warnings denied passed on macOS.
- macOS app bundle built, ad-hoc signature verified, and GUI launch tested.
- Tests cover refused overwrites, wrong-key rejection, forgotten keys on view
  changes, recoverable errors, and exact restored bytes.
- Local screenshots and detailed logs live under the project's dated evidence
  directory. No backup keys or real source images are included in Git.
- Mac builds are not Developer ID signed or notarized. Windows builds remain
  unsigned. These limitations are disclosed in the release notes.
- No multi-vdev or RAIDZ support was added. Live production pools were not
  exported, unmounted, stopped, or modified for this work.
