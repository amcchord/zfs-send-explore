# Desktop application icons

The Mac and Windows clients share a layered-storage and magnifying-glass mark.
The native resources are checked in so regular builds need no image tooling.

- `ZFSExplore.icns`: 16–1024 pixel representations for Finder, Dock and Retina.
- `ZFSExplore.ico`: 16, 20, 24, 32, 40, 48, 64, 96, 128 and 256 pixel RGBA frames
  for Explorer, the taskbar, window chrome and common Windows DPI scales.

The approved PNG master and generation prompt are retained in the parent
workspace's `artifacts/desktop-icons/`, outside the product checkout. The artwork
was created using the built-in image generator for this project on 2026-09-22.
Only the native resources needed to build the apps are product assets.

To regenerate on a Mac (Python 3 plus macOS `sips` and `iconutil`):

```sh
python3 scripts/package-icons.py /absolute/path/to/zfs-explore-master.png
```

macOS references the ICNS in `CFBundleIconFile`; Windows embeds icon group 101
and loads separate large and small class icons from that group. Keep the group
ID in the resource script and Win32 client synchronized.
