#!/usr/bin/env python3
"""Verify a complete app after copying it to a separate Applications directory."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("app", type=Path)
parser.add_argument("--work-dir", type=Path, default=root / "target/package-verification")
args = parser.parse_args()
args.work_dir.mkdir(parents=True, exist_ok=True)

with tempfile.TemporaryDirectory(prefix="relocated-", dir=args.work_dir.resolve()) as temporary:
    work = Path(temporary)
    app = work / "Applications/ZFS Explore.app"
    app.parent.mkdir()
    subprocess.run(["ditto", str(args.app.resolve()), str(app)], check=True)
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
    info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    icon = app / "Contents/Resources" / info["CFBundleIconFile"]
    assert icon.read_bytes()[:4] == b"icns", "App icon is missing or invalid"
    executable = app / "Contents/MacOS" / info["CFBundleExecutable"]
    service = app / "Contents/MacOS/zfs-explore-service"
    for binary in (executable, service):
        assert binary.is_file() and os.access(binary, os.X_OK), binary
        # A copied app must not depend on Homebrew, a checkout, or build-machine
        # Swift libraries. The current app deliberately uses only OS libraries.
        linked = subprocess.check_output(["otool", "-L", str(binary)], text=True)
        for line in linked.splitlines()[1:]:
            library = line.strip().split(" (", 1)[0]
            assert library.startswith(("/System/Library/", "/usr/lib/")), linked

    destination = work / "restored-hello.txt"
    expected = b"hello from the base snapshot\n"
    requests = [
        {"method": "open", "path": str(root / "tests/fixtures/tiny-full.zfs")},
        {"method": "extract", "name": "hello.txt", "destination": str(destination)},
    ]
    # There is no adjacent CLI, build directory, Rust, Homebrew or ZFS on PATH.
    response = subprocess.run([str(service)], cwd=app.parent,
                              env={"PATH": "/usr/bin:/bin", "TMPDIR": str(work)},
                              input="".join(json.dumps(r) + "\n" for r in requests),
                              text=True, capture_output=True, timeout=30, check=True)
    messages = [json.loads(line) for line in response.stdout.splitlines()]
    assert len(messages) == 2 and all(m["ok"] for m in messages), messages
    assert destination.read_bytes() == expected, "Relocated worker restore differs"
    print(json.dumps({"relocated_app": str(app), "signature_valid": True,
                      "icon": icon.name, "dependencies": "macOS system libraries only",
                      "worker_restore_sha256": hashlib.sha256(expected).hexdigest()}, indent=2))
