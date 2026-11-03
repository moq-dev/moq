#!/usr/bin/env bash
# Run moq-video's ignored MediaCodec tests on the Android device or emulator adb
# reaches.
#
# MediaCodec is a service on the device, so nothing on the host can run these.
# The test binary is cross-compiled for the device's ABI, and cargo hands it to
# this same script (`--run`) as its target runner, which pushes it over adb and
# runs it there. Any device works: a phone with USB debugging, a local emulator,
# or android.yml's x86_64 one. Software codecs are enough, so an emulator's
# Codec2 `c2.android.avc.*` pair passes.
#
# `ANDROID_ABI` skips asking the device, so `ANDROID_ABI=x86_64 just rs
# android-test --no-run` compiles before any device is up. Arguments go to
# `cargo test`, ahead of the test filter.
set -euo pipefail

if [[ "${1:-}" == --run ]]; then
    bin=$2
    shift 2
    dest=/data/local/tmp/$(basename "$bin")
    adb push "$bin" "$dest" >/dev/null
    # adb joins its arguments into one remote shell command, so quote each one.
    # Since Android 7 its exit status is the test binary's.
    exec adb shell "$(printf '%q ' "$dest" "$@")"
fi

cd "$(git rev-parse --show-toplevel)"

abi=${ANDROID_ABI:-$(adb shell getprop ro.product.cpu.abi | tr -d '\r')}
case "$abi" in
    arm64-v8a) triple=aarch64-linux-android ;;
    x86_64) triple=x86_64-linux-android ;;
    *)
        echo "Unsupported Android ABI: '$abi'" >&2
        exit 1
        ;;
esac

runner=CARGO_TARGET_$(tr 'a-z-' 'A-Z_' <<<"$triple")_RUNNER
export "$runner=$PWD/sh/rs/android-test.sh --run"

# API 26 is the floor the `mediacodec` feature builds against (see moq-video's
# Cargo.toml), matching `just rs android`.
exec bash sh/local.sh cargo ndk -t "$abi" --platform 26 test --locked -p moq-video --lib "$@" \
    -- --ignored backend::mediacodec::tests::
