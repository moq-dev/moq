#!/usr/bin/env bash
set -euo pipefail

# Generation-only step for the Kotlin wrapper: build moq-ffi for the host
# target, drop the cdylib into the JNA resource layout of the :moq KMP module,
# and regenerate the uniffi bindings. No Gradle, no JDK required.
#
# Intended for environments that intentionally lack Gradle (e.g. regenerating
# checked-in bindings after a moq-ffi change). `scripts/check.sh` calls this
# first and then compiles + tests the result. Requires cargo.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$KT_DIR/.." && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
    echo "kt generate: cargo not on PATH; install the Rust toolchain (or use 'nix develop')" >&2
    exit 1
fi
if ! command -v rustc >/dev/null 2>&1; then
    echo "kt generate: rustc not on PATH; install the Rust toolchain (or use 'nix develop')" >&2
    exit 1
fi

HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
echo "kt generate: building moq-ffi for $HOST_TARGET..."
cargo build --release --package moq-ffi \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml"

TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE_DIR/Cargo.toml" --no-deps |
    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

case "$HOST_TARGET" in
    *-apple-*)
        CDYLIB="$TARGET_BASE/release/libmoq_ffi.dylib"
        OS_TAG="darwin"
        ;;
    *-windows-*)
        CDYLIB="$TARGET_BASE/release/moq_ffi.dll"
        OS_TAG="win32"
        ;;
    *)
        CDYLIB="$TARGET_BASE/release/libmoq_ffi.so"
        OS_TAG="linux"
        ;;
esac
case "$HOST_TARGET" in
    aarch64-*) ARCH_TAG="aarch64" ;;
    x86_64-*) ARCH_TAG="x86-64" ;;
    *)
        echo "kt generate: unsupported host arch in $HOST_TARGET" >&2
        exit 1
        ;;
esac

[[ -f "$CDYLIB" ]] || {
    echo "kt generate: cdylib not found at $CDYLIB" >&2
    exit 1
}

RES_DIR="$KT_DIR/moq/src/jvmMain/resources/${OS_TAG}-${ARCH_TAG}"
mkdir -p "$RES_DIR"
cp "$CDYLIB" "$RES_DIR/"

BINDGEN_OUT=$(mktemp -d)
trap 'rm -rf "$BINDGEN_OUT"' EXIT
cargo run --release --package moq-ffi --bin uniffi-bindgen \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml" -- \
    generate --library "$CDYLIB" --language kotlin --no-format --out-dir "$BINDGEN_OUT"

mkdir -p "$KT_DIR/moq/src/jvmAndAndroidMain/kotlin/uniffi/moq"
cp "$BINDGEN_OUT/uniffi/moq/moq.kt" "$KT_DIR/moq/src/jvmAndAndroidMain/kotlin/uniffi/moq/moq.kt"
