#!/usr/bin/env bash
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${MOQ_LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

# The simulators have no hardware encoder, so the release build (rs/moq-ffi/build.sh)
# turns on `openh264`; check the same feature set it ships.
for target in aarch64-apple-ios-sim x86_64-apple-ios; do
    rustup target add "$target"
    if ! cargo tree --locked --package moq-ffi --features openh264 --target "$target" | grep -q "openh264-sys2 v"; then
        echo "::error::openh264 must stay enabled for Apple simulator builds"
        exit 1
    fi
    cargo check --locked --package moq-ffi --features openh264 --target "$target"
done
