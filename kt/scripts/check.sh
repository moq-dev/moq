#!/usr/bin/env bash
set -euo pipefail

# Local smoke check for the Kotlin wrapper.
#
# Builds moq-ffi for the host target, drops the cdylib into the JNA-resource
# layout of the :moq KMP module, regenerates the bindings, and runs
# `:moq:jvmTest`. Intended for `just check-ffi`.
#
# Skipped cleanly on hosts without `java` or `cargo`.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$KT_DIR/.." && pwd)"

if ! command -v java >/dev/null 2>&1; then
    echo "kt check: no JDK on PATH, skipping" >&2
    exit 0
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "kt check: no cargo on PATH, skipping" >&2
    exit 0
fi

HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
echo "kt check: building moq-ffi for $HOST_TARGET..."
cargo build --release --package moq-ffi \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml"

TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE_DIR/Cargo.toml" --no-deps \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

case "$HOST_TARGET" in
    *-apple-*) CDYLIB="$TARGET_BASE/release/libmoq_ffi.dylib"; OS_TAG="darwin";;
    *-windows-*) CDYLIB="$TARGET_BASE/release/moq_ffi.dll"; OS_TAG="win32";;
    *) CDYLIB="$TARGET_BASE/release/libmoq_ffi.so"; OS_TAG="linux";;
esac
case "$HOST_TARGET" in
    aarch64-*) ARCH_TAG="aarch64";;
    x86_64-*) ARCH_TAG="x86-64";;
    *) echo "kt check: unsupported host arch in $HOST_TARGET" >&2; exit 1;;
esac

[[ -f "$CDYLIB" ]] || { echo "kt check: cdylib not found at $CDYLIB" >&2; exit 1; }

RES_DIR="$KT_DIR/moq/src/jvmMain/resources/${OS_TAG}-${ARCH_TAG}"
mkdir -p "$RES_DIR"
cp "$CDYLIB" "$RES_DIR/"

BINDGEN_OUT=$(mktemp -d)
trap 'rm -rf "$BINDGEN_OUT"' EXIT
cargo run --release --package moq-ffi --bin uniffi-bindgen \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml" -- \
    generate --library "$CDYLIB" --language kotlin --out-dir "$BINDGEN_OUT"

mkdir -p "$KT_DIR/moq/src/jvmAndAndroidMain/kotlin/uniffi/moq"
cp "$BINDGEN_OUT/uniffi/moq/moq.kt" "$KT_DIR/moq/src/jvmAndAndroidMain/kotlin/uniffi/moq/moq.kt"

GRADLE_CMD="${GRADLE_CMD:-$(command -v gradle || true)}"
if [[ -z "$GRADLE_CMD" ]]; then
    echo "kt check: gradle not on PATH, skipping" >&2
    exit 0
fi

"$GRADLE_CMD" -p "$KT_DIR" -Pmoqffi.version=0.0.0-dev :moq:jvmTest
