#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p target/macos-drive-tests
swiftc -o target/macos-drive-tests/check macos/AttachedDrives.swift tests/macos/AttachedDriveTests.swift
target/macos-drive-tests/check
