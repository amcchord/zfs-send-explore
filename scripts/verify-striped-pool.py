#!/usr/bin/env python3
"""Check native stripe recovery against mounted OpenZFS source hashes (no pool import)."""
import argparse
import hashlib
import json
import subprocess
from pathlib import Path


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def hashes(path):
    entries = {}
    for line in path.read_text().splitlines():
        digest, name = line.split(maxsplit=1)
        entries[name] = digest
    return entries


def verify(binary, fixture, out, key_file, foreign):
    out.mkdir(parents=True, exist_ok=False)
    pool = (fixture / "pool-name").read_text().strip()
    dataset = f"{pool}/files"
    left, right = (fixture / "members" / name for name in ("left.img", "right.img"))

    def run(label, *command, expect_error=False):
        args = [str(binary), "pool", *map(str, command)]
        result = subprocess.run(args, capture_output=True, text=True, errors="replace")
        (out / f"{label}.log").write_text(
            json.dumps(args) + "\n" + result.stdout + result.stderr, encoding="utf-8")
        if expect_error != (result.returncode != 0):
            raise RuntimeError(f"{label} returned {result.returncode}: {result.stdout}{result.stderr}")
        return result.stdout

    def check_file(path, expected):
        digest = sha256(path)
        if digest != expected:
            raise RuntimeError(f"{path} has SHA-256 {digest}, expected {expected}")
        print(f"{digest}  {path.name}")

    def check_image_hashes():
        for name, expected in hashes(fixture / "members.sha256").items():
            check_file(fixture / name, expected)

    def rejects(label, member_args):
        target = out / f"{label}.bin"
        run(label, "extract", *member_args, f"{dataset}@late", "/across.bin",
            "--output", target, expect_error=True)
        if target.exists() or Path(str(target) + ".zfse.json").exists():
            raise RuntimeError(f"{label} published a file or sidecar")

    check_image_hashes()
    run("layout", "layout", left, right, "--json")
    metadata = json.loads(run("inspect", "inspect", right, "--member", left, "--json"))
    if metadata["pool_name"] != pool or metadata["top_level_vdevs"] != 2:
        raise RuntimeError("Incorrect pool identity or top-level group count")
    catalog = run("datasets", "datasets", left, "--member", right)
    if dataset not in catalog or f"{pool}/locked" not in catalog:
        raise RuntimeError("One or more datasets are missing")
    snapshots = run("snapshots", "snapshots", right, dataset, "--member", left)
    if "@early" not in snapshots or "@late" not in snapshots:
        raise RuntimeError("One or more named snapshots are missing")
    listing = run("list", "list", left, f"{dataset}@early", "/", "--member", right)
    if "across.bin" not in listing or "documents" not in listing:
        raise RuntimeError("Wrong snapshot directory")
    for name, primary, other in [("early", left, right), ("late", right, left)]:
        snap = f"{dataset}@{name}"
        originals = hashes(fixture / f"{name}.sha256")
        large = out / f"{name}.bin"
        run(f"{name}-large", "extract", primary, snap, "/across.bin", "-o", large, "--member", other)
        check_file(large, originals["across.bin"])
        if not Path(str(large) + ".zfse.json").is_file():
            raise RuntimeError("Missing named-snapshot file sidecar")
        tree = out / f"{name}-documents"
        run(f"{name}-tree", "extract-tree", primary, snap, "/documents", "-o", tree, "--member", other)
        check_file(tree / "revision.txt", originals["documents/revision.txt"])
        check_file(tree / "nested/note.txt", originals["documents/nested/note.txt"])

    encrypted = out / "private.txt"
    run("encrypted", "extract", left, f"{pool}/locked@early", "/private.txt", "-o", encrypted,
        "--key-file", key_file, "--member", right)
    check_file(encrypted, hashes(fixture / "locked.sha256")["private.txt"])

    inner = out / "hostname"
    run("nested-inspect", "inception", "inspect", right, f"{dataset}@late", "/guest.img", "--member", left)
    run("nested", "inception", "extract", right, f"{dataset}@late", "/guest.img", "/etc/hostname",
        "-o", inner, "--member", left)
    check_file(inner, hashes(fixture / "nested.sha256")["etc/hostname"])

    rejects("missing-left", [right])
    rejects("missing-right", [left])
    rejects("duplicate", [left, "--member", left])
    for name, member in [("left", left), ("right", right)]:
        # Replacing either supplied image through an alternate name must fail.
        alias = out / f"{name}-member-alias"
        alias.hardlink_to(member)
        run(f"protect-{name}", "extract", left, f"{dataset}@late", "/across.bin",
            "--member", right, "-o", alias, "--force", expect_error=True)
        if not alias.samefile(member):
            raise RuntimeError(f"The restore replaced a {name} source alias")
        alias.unlink()
    if foreign:
        rejects("mixed", [left, "--member", foreign])
    check_image_hashes()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("fixture", type=Path)
    parser.add_argument("new_output", type=Path)
    parser.add_argument("--key-file", type=Path, help="The generated fixture key (defaults to FIXTURE/dataset.key)")
    parser.add_argument("--foreign-member", type=Path, help="Also test rejection of a member from a different pool")
    args = parser.parse_args()
    verify(args.binary.resolve(), args.fixture, args.new_output,
           args.key_file or args.fixture / "dataset.key", args.foreign_member)
