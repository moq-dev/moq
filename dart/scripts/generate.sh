#!/usr/bin/env bash
set -euo pipefail

DART_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORKSPACE_DIR=$(cd "$DART_DIR/.." && pwd)
OUTPUT_DIR="${1:-$DART_DIR/moq_ffi/lib/src}"

command -v cargo >/dev/null 2>&1 || {
    echo "error: cargo is required" >&2
    exit 1
}
command -v uniffi_bindgen_dart >/dev/null 2>&1 || {
    echo "error: uniffi_bindgen_dart is required" >&2
    exit 1
}
command -v dart >/dev/null 2>&1 || {
    echo "error: dart is required" >&2
    exit 1
}

host=$(rustc -vV | sed -n 's/^host: //p')
cargo build --locked --release --package moq-ffi --no-default-features \
    --target "$host" \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml"

case "$host" in
    *-apple-*) library="$WORKSPACE_DIR/target/$host/release/libmoq_ffi.dylib" ;;
    *-windows-*) library="$WORKSPACE_DIR/target/$host/release/moq_ffi.dll" ;;
    *) library="$WORKSPACE_DIR/target/$host/release/libmoq_ffi.so" ;;
esac

mkdir -p "$OUTPUT_DIR"
rm -f "$OUTPUT_DIR/moq.dart" "$OUTPUT_DIR/uniffi_runtime.dart"
uniffi_bindgen_dart --library "$library" \
    --config "$DART_DIR/uniffi.toml" \
    --out-dir "$OUTPUT_DIR"

# Generated identifiers mirror Rust and C names, which intentionally do not
# follow Dart's style lints. Keep analyzer type errors active without emitting
# hundreds of unactionable generated-code lint messages.
for file in "$OUTPUT_DIR/moq.dart" "$OUTPUT_DIR/uniffi_runtime.dart"; do
    sed -i.bak '1s/unused_import/unused_import, type=lint/' "$file"
    rm -f "$file.bak"
done
dart format --language-version 3.10 "$OUTPUT_DIR/moq.dart" "$OUTPUT_DIR/uniffi_runtime.dart"
