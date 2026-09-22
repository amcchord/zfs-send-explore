# macOS client

ZFS Explore is a native SwiftUI app for macOS 13 or later, with the same Rust
recovery engine as the CLI and Windows app. No ZFS driver or pool mount is needed.

Build on a Mac with Xcode command-line tools and Rust 1.87 or later:

```sh
./scripts/package-macos.sh
open 'target/macos-package/ZFS Explore.app'
```

The package directory contains the app, standalone `zfs-send-extract` CLI,
archives, and checksums. `ZFSE_MACOS_ARCH=arm64` (Apple Silicon) or `x86_64`
(Intel) selects an architecture; the matching Rust target must be installed.
Builds are ad-hoc signed, not Developer ID signed or notarized. Public customer
release still requires a signing identity and Apple notarization.

![ZFS Explore on macOS](screenshots/macos-welcome.png)

## Recover a file

1. Choose **Open Backup** (⌘O) and select a ZFS send, exported pool `.img`, or
   standalone disk image. **Open Path** also accepts a pasted absolute path.
2. Select the intended snapshot or dataset in the sidebar.
3. For encrypted backups, use **Enter Key** or **Choose Key File**. Slide's
   64-character raw key and 32-byte binary key files are both accepted.
4. Double-click folders. For `disk_*.raw`, double-click the image or select
   **Explore as Disk Image**, then choose the filesystem volume if necessary.
   Use **Back to outer backup** to return without reopening the source.
5. Select a regular file or folder and choose **Restore…** (⌘S). Pick a new
   destination. Existing files are never overwritten by the Mac app.
6. The completion panel shows the destination, byte count, and SHA-256 for a
   single file. **Show in Finder** reveals the result. Folder recovery reports
   skipped symlinks/special files and does not restore filesystem metadata.

Keys travel through a private child-process pipe, never command-line arguments
or a local HTTP server. The Rust engine holds key bytes in zeroizing memory.
Changing snapshots, closing a backup, or quitting discards the active key.
Swift secure-field contents are cleared on submit/dismiss; Swift/Foundation
string copies are not guaranteed to be zeroized.

The Mac app supports ZFS native encryption; Datto LUKS and agent-password UI,
physical disk discovery, and incremental application remain CLI/Windows features.
A directly supplied device path must already be readable by the current user.
Use an offline image for normal recovery. Single disk or single top-level mirror
pool layouts are supported; RAIDZ and striped/multiple-vdev pools are not.

Long operations show an indeterminate activity indicator and run off the main
thread. Byte-level progress and cancel/resume are not yet provided.
