# Windows UI/UX review and v0.5.0–v0.5.1 design

This pass treats the Windows client as a recovery tool for two audiences: an operator who has a backup or drive and wants the safest obvious next action, and a power user who needs exact source selection, nested-image control, and keyboard speed. The backend safety model remains unchanged: sources are read-only, untrusted structures are bounded and validated, and extraction publishes only after successful completion.

## v0.5.1 contextual-action follow-up

The follow-up review applies one rule throughout the UI: when the app knows the action needed to continue, that action is offered at that point instead of being described as something to find elsewhere.

| Friction found after v0.5.0 | v0.5.1 response |
| --- | --- |
| The encrypted-view prompt said a binary raw key must be chosen from a file, but supplied no file action. | A native command-link dialog reads the configured `raw`, `hex`, or `passphrase` key format and places direct entry and the appropriate file chooser together. Raw datasets explicitly distinguish Slide's 64-character representation from an original 32-byte binary file. |
| A missing or incorrect LUKS pool passphrase, ZFS key, or `.detto` agent password ended as a generic operation error. | The relevant direct/file choice reopens and the interrupted source, browse, or image operation retries after replacement. |
| One ZFS key could be reused accidentally when switching encrypted datasets. | Retained ZFS keys are keyed by source and view selector; unrelated datasets do not silently receive one another's key material. |
| Starting a new source cleared the working source and its credentials before validation. | Source replacement is transactional: the current catalog and listing remain active until the new source opens successfully. Scoped credentials are committed or cleared only after success. |
| **Ctrl+O** and **Ctrl+Shift+O** could reuse text already present in the source box instead of showing a chooser. | Both accelerators always open their documented picker. **Choose** selects and opens in one step; **Open path** is the explicit action for pasted/manual paths. |
| The image row showed blank disabled controls outside inception mode, and two unlabeled number boxes inside it. | Image-only navigation appears only inside an image. The Open-image action moves beside Extract, while advanced range fields are hidden by default and labeled when enabled. |
| The active image's offset and size were carried into the next selected child image. | Child range fields are cleared after every successful descent. They now describe only the next selected file. |
| Clearing credentials closed an active source without first exposing that consequence. | A warning command link explicitly says **Clear credentials and close source** before releasing the retained secrets and read session. |

## What changed

| Problem in v0.4.0 | v0.5.0 response |
| --- | --- |
| Users had to decide whether a path was a send stream or pool member before opening it. | **Open** probes the supported formats and reports the diagnostics from every attempted reader if none match. Explicit “open as send” and image-specific commands remain in the File menu. |
| A physical drive had to be typed as `\\.\PhysicalDriveN`. | The app enumerates zero-access physical-device handles and shows disk number, storage model, capacity, and fixed/removable type in a picker. **F5** refreshes it. A confirmation repeats the exact selection before read-only opening. |
| ZFS, LUKS, and Datto secrets were separate file-only buttons. | One **Credentials** surface supports masked native Windows entry and bounded key files for all three roles. Direct input never touches disk; loaded bytes are retained in zeroizing memory. |
| Selecting an encrypted view failed first and then explained how to add a key. | An encrypted view requests its key immediately. Binary raw keys still use a file, while passphrase and hex formats can be pasted directly. |
| Disk-image exploration was hidden behind a button and stopped after one level. | Recognized images open with double-click or **Enter**, standalone images can be opened directly, and an image inside NTFS/FAT/exFAT/ext can be opened as the next layer without exporting it. |
| Leaving inception mode lost navigation context. | **Back one image** restores the exact parent directory and volume. A breadcrumb shows source, ZFS view, every image layer, and current path. |
| The third and fourth toolbar rows mixed advanced image fields with unrelated credentials. | Source, drive, credentials, view/path, breadcrumb, and image controls now have one consistent top-to-bottom task order. |
| Menu items did not provide a keyboard workflow. | Native accelerators cover opening, drive access, credentials, path focus, image navigation, extraction, updates, and refresh. List **Enter** and **Backspace** follow Explorer conventions. |
| Preferences were absent. | The Settings menu persists image double-click behavior, source-scoped credential clearing, physical-drive confirmation, and window size. No secret or source content is persisted. |

## Primary workflow

![The v0.5.0 source and snapshot browser](screenshots/windows/source-browser.png)

The first decision is now “file or physical drive,” not “send parser or pool parser.” **Choose** selects and immediately opens a file; **Open path** handles a pasted/manual source. A device is selected by observable hardware identity, then confirmed and opened. Once a source is recognized, the view selector, breadcrumb, path controls, and file list follow the same interaction model for streams, pools, and nested filesystems.

The app keeps advanced controls available without making them prerequisites. Image offset and length are hidden for the normal whole-file case and can be revealed from Settings or directly from an image-open failure. Exact manual paths, format-specific open commands, volume selectors, and every legacy key-file format remain available.

## Credential model

![The format-aware credential method chooser](screenshots/windows/credential-method.png)

The method chooser leads to either the relevant file dialog or the native secure credential prompt:

![The native secure credential prompt](screenshots/windows/credential-entry.png)

Three credentials have deliberately distinct roles:

- **ZFS dataset key** authenticates raw sends and native-encrypted ZFS datasets. It may be a passphrase, hexadecimal key, 32-byte raw file, or Slide's 64-character raw-key representation.
- **Datto pool passphrase** unlocks the outer LUKS1/LUKS2 Reverse RoundTrip partition.
- **Datto agent password** authenticates an agent's `.encryptionKeyStash` and derives the `.detto` AES-XTS key.

The native prompt disables Windows credential persistence. Direct UTF-16 input is converted to bytes, the temporary UTF-16 and string buffers are zeroized, and the retained bytes use `Zeroizing<Vec<u8>>`. Key-file reads are size-bounded before allocation. ZFS keys are scoped to source plus view; pool and agent credentials are scoped to source. With the default setting, unrelated credentials are dropped only after another source opens successfully. **Credentials > Clear all credentials** first confirms that the active source will close, then releases every retained value, including an outer pool-unlock key owned by the open read session.

## Recursive inception model

![A standalone filesystem image in inception mode](screenshots/windows/standalone-image.png)

An inception layer consists of a bounded image window, a detected raw/QCOW2/VMDK container, validated partition metadata, and a selected supported filesystem. A child regular file is exposed as another positioned `ImageRead` implementation. FAT uses cluster-aware `read_at`; NTFS and ext reopen and seek the selected file for requested ranges. The child parser therefore reads only the needed sectors rather than materializing the entire image.

The UI retains at most eight active image layers. Each frame records the parent path and selected parent volume. Going back pops one frame and restores both values. Standalone images use the same layer stack but close to the empty start state when the first frame is left.

Known format limits remain intentional: QCOW1, external/backing QCOW2, split/flat/stream-optimized VMDK, NTFS compression, EFS, and non-UTF-8 ext names are rejected. VHD/VHDX extensions are recognized as likely images for discoverability, but the container parser reports that they are unsupported rather than guessing a raw layout.

## Keyboard model

The full shortcut table is in the [Windows client guide](windows-client.md#keyboard-shortcuts-and-settings). Important conventions are **Ctrl+O** to choose and open, **Ctrl+K** for contextual credentials, **Ctrl+L** for location, **Alt+Up/Backspace** for a parent, **Enter** to open, **Esc** to leave one image, **Ctrl+E** to extract, and **F5** to refresh. Shortcuts are implemented as native accelerator-table commands; list Enter/Backspace use list-view key notifications so they do not hijack typing in edit fields.

## Validation and acceptance

- Native unit/integration suite: 61 tests across library, CLI, encryption, and the Windows-facing service layer.
- Key-format regression: the Windows-facing catalog exposes `raw`, `hex`, or `passphrase` before encrypted traversal so the lock-point dialog can present exact actions.
- New nested-image test: a FAT image containing a second complete FAT image is opened and listed without exporting either image.
- Native clippy: all targets with warnings denied.
- Windows x86-64 clippy: all targets with warnings denied.
- Windows release build and packaging: performed by CI on a native Windows runner.
- Screenshot capture: optimized executable, real Win32 controls, real Windows credential UI, committed send fixture, and committed ext4 fixture.

Hardware acceptance is separate from UI correctness. The existing direct-member validation covers a physical exported vdev. A representative physical Slide Box and Datto Reverse RoundTrip drive are still recommended before broad operational deployment, especially for unsupported multi-vdev/RAIDZ layouts.
