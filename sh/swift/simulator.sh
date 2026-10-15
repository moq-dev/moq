#!/usr/bin/env bash
set -euo pipefail

for target in aarch64-apple-ios-sim x86_64-apple-ios; do
    rustup target add "$target"
    if ! cargo tree --locked --package moq-ffi --target "$target" | grep -q "openh264-sys2 v"; then
        echo "::error::openh264 must stay enabled for Apple simulator builds"
        exit 1
    fi
    cargo check --locked --package moq-ffi --target "$target"
done
