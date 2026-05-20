#!/usr/bin/env bash
set -euo pipefail

# Local smoke check for the Kotlin wrappers.
#
# Builds moq-ffi for the host target, copies the cdylib + uniffi-generated
# Kotlin source into the appropriate places, and runs `:moq-jvm:test`.
#
# Skipped on hosts without a JDK or without `cargo` (just prints and exits 0).
# Intended for `just check-ffi` so the inner loop stays optional.

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

# --- Build moq-ffi for host ---
HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
echo "kt check: building moq-ffi for $HOST_TARGET..."
cargo build --release --package moq-ffi \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml"

TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE_DIR/Cargo.toml" --no-deps \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

# Locate the cdylib for this host.
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

# --- Place host lib at JNA's expected resource path ---
RES_DIR="$KT_DIR/moq-jvm/src/main/resources/${OS_TAG}-${ARCH_TAG}"
mkdir -p "$RES_DIR"
cp "$CDYLIB" "$RES_DIR/"

# --- Generate Kotlin bindings ---
BINDGEN_OUT=$(mktemp -d)
trap 'rm -rf "$BINDGEN_OUT"' EXIT
cargo run --release --package moq-ffi --bin uniffi-bindgen \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml" -- \
    generate --library "$CDYLIB" --language kotlin --out-dir "$BINDGEN_OUT"

mkdir -p "$KT_DIR/common/src/uniffi/moq"
cp "$BINDGEN_OUT/uniffi/moq/moq.kt" "$KT_DIR/common/src/uniffi/moq/moq.kt"

# --- Run gradle ---
GRADLE_CMD="${GRADLE_CMD:-$(command -v gradle || true)}"
if [[ -z "$GRADLE_CMD" ]]; then
    echo "kt check: gradle not on PATH and no wrapper available, skipping test" >&2
    exit 0
fi

"$GRADLE_CMD" -p "$KT_DIR" -Pmoqffi.version=0.0.0-dev :moq-jvm:test
