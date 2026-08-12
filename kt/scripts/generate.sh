#!/usr/bin/env bash
set -euo pipefail

# Generation-only step for the Kotlin packages: build moq-ffi for the host
# target, drop the cdylib into the JNA resource layout of the :moq-ffi KMP module,
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

echo "kt generate: building moq-ffi for $HOST_TARGET..."
cargo build ${CARGO_PROFILE[@]+"${CARGO_PROFILE[@]}"} --package moq-ffi \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml"

TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE_DIR/Cargo.toml" --no-deps |
    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

case "$HOST_TARGET" in
    *-apple-*)
        CDYLIB="$TARGET_BASE/$PROFILE/libmoq_ffi.dylib"
        OS_TAG="darwin"
        ;;
    *-windows-*)
        CDYLIB="$TARGET_BASE/$PROFILE/moq_ffi.dll"
        OS_TAG="win32"
        ;;
    *)
        CDYLIB="$TARGET_BASE/$PROFILE/libmoq_ffi.so"
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

RES_DIR="$KT_DIR/moq-ffi/src/jvmMain/resources/${OS_TAG}-${ARCH_TAG}"
mkdir -p "$RES_DIR"
cp "$CDYLIB" "$RES_DIR/"

BINDGEN_OUT=$(mktemp -d)
trap 'rm -rf "$BINDGEN_OUT"' EXIT
cargo run ${CARGO_PROFILE[@]+"${CARGO_PROFILE[@]}"} --package moq-ffi --bin uniffi-bindgen \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml" -- \
    generate --library "$CDYLIB" --language kotlin --no-format --out-dir "$BINDGEN_OUT"

mkdir -p "$KT_DIR/moq-ffi/src/jvmAndAndroidMain/kotlin/uniffi/moq"
cp "$BINDGEN_OUT/uniffi/moq/moq.kt" "$KT_DIR/moq-ffi/src/jvmAndAndroidMain/kotlin/uniffi/moq/moq.kt"
