#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p target/macos-time-tests
swiftc -o target/macos-time-tests/check macos/SnapshotTime.swift tests/macos/SnapshotTimeTests.swift
target/macos-time-tests/check
