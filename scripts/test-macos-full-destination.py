#!/usr/bin/env python3
"""Check incremental recovery on a full, disposable HFS+ volume."""

import errno
import hashlib
import json
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
CLI = ROOT / "target/debug/zfs-send-extract"
FIXTURES = ROOT / "tests/fixtures"


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fill_volume(volume, work):
    fs = os.statvfs(volume)
    if volume.stat().st_dev == work.stat().st_dev or fs.f_blocks * fs.f_frsize > 32 * 1024**2:
        raise RuntimeError("Expected the newly attached 32 MiB test image")
    with (volume / "filler").open("xb", buffering=0) as filler:
        for size in (1024 * 1024, 4096):
            block = b"x" * size
            while True:
                try:
                    filler.write(block)
                except OSError as error:
                    if error.errno != errno.ENOSPC:
                        raise
                    break
        os.fsync(filler.fileno())


def check_restore(volume, work):
    target = volume / "hello.txt"
    sidecar = volume / "hello.txt.zfse.json"
    subprocess.run([CLI, "extract", FIXTURES / "tiny-full.zfs", "/hello.txt", "-o", target], check=True)
    previous = (sha256(target), sha256(sidecar))
    fill_volume(volume, work)
    update = [CLI, "apply", FIXTURES / "tiny-incremental.zfs", target]
    failed = subprocess.run(update, capture_output=True, text=True)
    assert failed.returncode != 0 and ("space" in failed.stderr.lower() or "os error 28" in failed.stderr.lower()), failed
    assert (sha256(target), sha256(sidecar)) == previous, "Full disk changed the original pair"
    (volume / "filler").unlink()
    subprocess.run(update, check=True)
    expected = b"hello from the incremental snapshot\nwith an appended line\n"
    assert target.read_bytes() == expected
    assert json.loads(sidecar.read_text())["sha256"] == sha256(target)
    print(f"Full-volume refusal preserved both originals. Retry restored SHA-256 {sha256(target)}.")


def main():
    with tempfile.TemporaryDirectory(prefix="zfse-full-destination-") as temporary:
        work = Path(temporary).resolve()
        volume = work / "volume"
        volume.mkdir()
        image = work / "test.dmg"
        subprocess.run(["hdiutil", "create", "-size", "32m", "-fs", "HFS+", "-volname", "ZFSE Test", image], check=True)
        attached = subprocess.check_output(["hdiutil", "attach", "-nobrowse", "-mountpoint", volume, "-plist", image])
        devices = plistlib.loads(attached)["system-entities"]
        device = next(entry["dev-entry"] for entry in devices if entry.get("mount-point") == str(volume))
        try:
            check_restore(volume, work)
        finally:
            subprocess.run(["hdiutil", "detach", device], check=True)


if __name__ == "__main__":
    main()
