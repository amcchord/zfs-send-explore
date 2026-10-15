# macOS client

ZFS Explore is a native SwiftUI app for macOS 14 or later, with the same Rust
recovery engine as the CLI and Windows app. No ZFS driver or pool mount is needed.

Build on a Mac with Xcode command-line tools and Rust 1.87 or later:

```sh
./scripts/package-macos.sh
open 'target/macos-package/ZFS Explore.app'
```

The package directory contains the app, standalone `zfs-send-extract` CLI,
archives, and checksums. `ZFSE_MACOS_ARCH=arm64` (Apple Silicon) or `x86_64`
(Intel) selects an architecture; the matching Rust target must be installed.
Local builds are ad-hoc signed. Published v0.6.1 Mac downloads are Developer ID
signed and Apple-notarized. Use the `.dmg` for your architecture to obtain both
the app and CLI; it carries a stapled notarization ticket, as does the app.
The CLI tarball has the same notarized executable but no stapled ticket on the
standalone binary, so prefer the disk image for offline preparation. Normal
first-open confirmation and organization device policies still apply.

For maintainers, see the [signing and notarization procedure](macos-signing.md).

## Install by dragging to Applications

Open the disk image and drag **ZFS Explore.app** onto **Applications**. Eject
the disk image, then open **ZFS Explore** from Applications. For a ZIP download,
unzip it first and drag the complete app to Applications.

The recovery engine and app icon are inside the app bundle. You do not need to
copy the standalone CLI, install Rust or Homebrew, install a ZFS driver, or keep
the download mounted. Keep the `.app` intact; do not move its internal files.

Packaging CI checks the icon and signatures, copies the app into a separate
Applications directory, checks for non-system dynamic dependencies, and restores
a fixture using only the copied app's bundled worker. Run this check locally:

```sh
python3 scripts/verify-macos-package.py 'target/macos-package/ZFS Explore.app'
```

![ZFS Explore on macOS](screenshots/macos-welcome.png)

## Recover a file

The window guides you through four steps, shown in the step bar at the top:
**Backup open → Restore point → Unlock → Find files → Restore**.

1. On the welcome screen choose **Open a backup file** (⌘O) or **Open a
   connected drive** (⇧⌘D; see below). You can also drag a backup file into the
   window, or use **Open from a path…** (⇧⌘O) for a pasted path such as
   `/Volumes/Backups/slide.img` or a device such as `/dev/disk4s2`.
2. The newest restore point is selected for you. Pick another date in the
   sidebar's **Restore points** list if you need an older copy. Live dataset
   states and parent datasets are under **Other places in this backup**.
3. If the restore point is encrypted, paste its key and choose **Unlock**, or
   choose **Choose Key File…**. Slide's 64-character key and its 32-byte key
   file both work. A wrong key is reported plainly and nothing else changes.
4. Browse with double-clicks and the clickable location bar. For a Slide or
   Datto backup, the banner **The computer's files are inside its disk image**
   offers **Open Disk Image**. When the disk has several parts, choose the one
   marked **Recommended** (it is the largest part that holds files).
5. Select a file or folder and choose **Restore…** (⌘S). The save panel starts
   on the Desktop. Existing files are never overwritten by the Mac app.
6. The **Restored** banner shows where the copy was saved; **Show in Finder**
   reveals it and **Details** shows the full path and SHA-256 of a single file.
   Folder restores report skipped links or special items and do not restore
   owners, permissions or timestamps.

Problems are explained in plain words with the engine's original message under
**Show Details**. Restore point dates are shown in your Mac's time zone; use the
link at the bottom of the sidebar or **ZFS Explore → Settings…** to change it.

Keys travel through a private child-process pipe, never command-line arguments
or a local HTTP server. The Rust engine holds key bytes in zeroizing memory.
Changing snapshots, closing a backup, or quitting discards the active key.
Swift secure-field contents are cleared on submit/dismiss; Swift/Foundation
string copies are not guaranteed to be zeroized.

## Open an attached drive

A drive holding a ZFS pool does not appear in Finder or in the Open Backup panel:
macOS cannot read its partitions, so it never mounts them. **Open a
connected drive** lists external disks directly from DiskArbitration instead, whether or
not Finder shows them, with each partition's type and size. A ZFS-typed
partition is suggested first, then the largest partition macOS left unmounted.
Choosing **Whole disk** also works when exactly one partition holds the pool.
The list refreshes every two seconds, so a drive connected while it is open
appears without reopening it. Internal storage and Apple disk images (such as
this app's own DMG) are hidden. Mounted partitions show their current mount path.
A suggestion is based on the partition type, not a guarantee that it contains ZFS.
Removing the selected device clears the selection; reconnecting requires choosing
it again, even if macOS reuses the same disk number.

Disk devices belong to `root:operator`, so macOS asks for an administrator
password when the selected device is not already readable. The engine runs
Apple's `/usr/libexec/authopen`, which shows the standard system prompt for the
right `sys.openfile.readonly./dev/diskN`, and passes back a read-only file
descriptor over a private socket. No privileged process stays running, the
device's permissions are not changed, and the handle is closed when the backup
is closed or another is opened. **Open Path** uses the same prompt for paths
under `/dev/` for exact disk nodes. Pool and standalone filesystem devices use
the same size detection and sector-aligned reads, including `/dev/rdiskN` paths.
The CLI needs an already readable device or an administrator launch; it does not
show the desktop authorization prompt.

The Mac app supports ZFS native encryption; Datto LUKS and agent-password UI and
incremental application remain CLI/Windows features. Single disk or single
top-level mirror pool layouts are supported; RAIDZ and striped/multiple-vdev
pools are not.

Long operations show an indeterminate activity indicator and run off the main
thread. Byte-level progress and cancel/resume are not yet provided.

## Snapshot dates

Snapshot creation times are shown as readable dates in your Mac's current time
zone by default. Pool snapshots are newest first. Click **Times** above the
snapshot list or use **ZFS Explore → Settings…** (Command-comma) to choose UTC
or search for another city/time zone. The preference persists across restarts.
The original snapshot ID remains beneath the date and in the hover detail.
Changing the display zone preserves your selected snapshot, browsing location
and in-memory key. Offsets are included to distinguish repeated hours at the
end of daylight saving time. Undated/current views retain their original label.

## NTFS compression

NTFS LZNT1-compressed files restore automatically in the same way as other files.
The shared Rust reader handles compressed, verbatim and sparse compression units,
fragmented attribute lists, partial final units and initialized-length zero fill.
Disk images stored as compressed NTFS files can also be browsed directly. Standard
16-cluster units with 512–4096-byte clusters are supported. EFS-encrypted streams
remain unsupported; this feature does not add WOF/CompactOS decompression.
