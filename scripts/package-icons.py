#!/usr/bin/env python3
"""Convert an approved PNG master to native app resources (macOS build host)."""

import argparse
from pathlib import Path
import struct
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("master", type=Path, help="PNG artwork, retained outside the checkout")
parser.add_argument("--output", type=Path, default=Path(__file__).resolve().parents[1] / "packaging/icons")
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)

# Keep conversion scratch inside the project, including when TMPDIR points elsewhere.
with tempfile.TemporaryDirectory(prefix=".icons-", dir=args.output) as temporary:
    root = Path(temporary)
    iconset = root / "ZFSExplore.iconset"
    iconset.mkdir()
    for points in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            pixels = points * scale
            suffix = "@2x" if scale == 2 else ""
            subprocess.run(["sips", "-z", str(pixels), str(pixels), str(args.master),
                            "--out", str(iconset / f"icon_{points}x{points}{suffix}.png")],
                           check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o",
                    str(args.output / "ZFSExplore.icns")], check=True)

    # Windows 10/11 supports PNG-compressed, full-alpha ICO frames. Include
    # intermediate DPI sizes as well as Explorer's 256px thumbnail.
    sizes = (16, 20, 24, 32, 40, 48, 64, 96, 128, 256)
    frames = []
    for pixels in sizes:
        path = root / f"windows-{pixels}.png"
        subprocess.run(["sips", "-z", str(pixels), str(pixels), str(args.master),
                        "--out", str(path)], check=True, stdout=subprocess.DEVNULL)
        frames.append(path.read_bytes())
    directory = bytearray(struct.pack("<HHH", 0, 1, len(sizes)))
    offset = 6 + 16 * len(sizes)
    for pixels, frame in zip(sizes, frames):
        directory.extend(struct.pack("<BBBBHHII", pixels % 256, pixels % 256,
                                     0, 0, 1, 32, len(frame), offset))
        offset += len(frame)
    (args.output / "ZFSExplore.ico").write_bytes(directory + b"".join(frames))

print(f"Created native icons in {args.output}")
