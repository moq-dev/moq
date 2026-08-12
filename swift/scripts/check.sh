#!/usr/bin/env bash
set -euo pipefail

# Local smoke check for the Swift wrapper. Builds moq-ffi for the host
# macOS target, lays out a local XCFramework, and runs `swift test`.
#
# Skipped on hosts without `swift` (Linux dev environments) or without
# `cargo`. Intended for `just swift check`.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SWIFT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$SWIFT_DIR/.." && pwd)"

if ! command -v swift >/dev/null 2>&1; then
    echo "swift check: no swift toolchain on PATH, skipping" >&2
    exit 0
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "swift check: no cargo on PATH, skipping" >&2
    exit 0
fi
if [[ "$(uname)" != "Darwin" ]]; then
    echo "swift check: not macOS, skipping" >&2
    exit 0
fi

# `nix develop` on Darwin exports SDKROOT pointing at the nixpkgs-bundled
# apple-sdk. The swift compiler in PATH (Xcode's, newer than that SDK)
# refuses to load the stdlib swiftmodules out of that SDK ("this SDK is
# not supported by the compiler"). Wrap swift/xcodebuild invocations
# with this so they fall back to `xcrun --show-sdk-path`, which resolves
# to Xcode's matching SDK. Cargo keeps the nix SDK so nix-toolchain
# clang/linker still resolve headers and libs from the dev shell.
xcode_sdk_env=()
if [[ "${SDKROOT:-}" == /nix/store/* ]]; then
    xcode_sdk_env+=(-u SDKROOT)
fi
if [[ "${DEVELOPER_DIR:-}" == /nix/store/* ]]; then
    xcode_sdk_env+=(-u DEVELOPER_DIR)
fi

HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
# Debug by default. This is a compile-and-test gate, not a benchmark, and a
# release build of moq-ffi shares no artifacts with the debug ones `just check`
# and `just test` already produce, so it was a third full compile of the
# dependency tree (~5 min of CI on its own, plus a whole target/release tree on
# a runner that was already tight on disk). Set MOQ_FFI_PROFILE=release for an
# optimized cdylib; the shipped artifacts are built by rs/moq-ffi/build.sh,
# which is release regardless.
PROFILE="${MOQ_FFI_PROFILE:-debug}"
# Expanded as ${CARGO_PROFILE[@]+...} at the use sites: macOS ships bash 3.2,
# where expanding an empty array under `set -u` is an "unbound variable" error.
CARGO_PROFILE=()
[[ "$PROFILE" == "release" ]] && CARGO_PROFILE=(--release)

echo "swift check: building moq-ffi for $HOST_TARGET..."
cargo build ${CARGO_PROFILE[@]+"${CARGO_PROFILE[@]}"} --package moq-ffi \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml"

TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE_DIR/Cargo.toml" --no-deps |
    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

CDYLIB="$TARGET_BASE/$PROFILE/libmoq_ffi.dylib"
STATIC="$TARGET_BASE/$PROFILE/libmoq_ffi.a"
[[ -f "$CDYLIB" && -f "$STATIC" ]] || {
    echo "swift check: missing $CDYLIB or $STATIC" >&2
    exit 1
}

# Generate bindings.
BINDGEN_OUT=$(mktemp -d)
trap 'rm -rf "$BINDGEN_OUT"' EXIT
cargo run ${CARGO_PROFILE[@]+"${CARGO_PROFILE[@]}"} --package moq-ffi --bin uniffi-bindgen \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml" -- \
    generate --library "$CDYLIB" --language swift --out-dir "$BINDGEN_OUT"

# Build a local XCFramework with just the host slice.
LOCAL_XCF="$SWIFT_DIR/MoqFFI.xcframework"
rm -rf "$LOCAL_XCF"
HEADERS_DIR="$BINDGEN_OUT/headers"
mkdir -p "$HEADERS_DIR"
cp "$BINDGEN_OUT/moqFFI.h" "$HEADERS_DIR/"
cp "$BINDGEN_OUT/moqFFI.modulemap" "$HEADERS_DIR/module.modulemap"

env ${xcode_sdk_env[@]+"${xcode_sdk_env[@]}"} xcodebuild -create-xcframework \
    -library "$STATIC" -headers "$HEADERS_DIR" \
    -output "$LOCAL_XCF"

# Stage generated swift.
mkdir -p "$SWIFT_DIR/Sources/MoqFFI"
cp "$BINDGEN_OUT/moq.swift" "$SWIFT_DIR/Sources/MoqFFI/Generated.swift"

# swift/Package.swift already declares a path-based MoqFFIBinary pointing
# at the xcframework laid out above, so no manifest mutation is needed.
cd "$SWIFT_DIR"
env ${xcode_sdk_env[@]+"${xcode_sdk_env[@]}"} swift test
