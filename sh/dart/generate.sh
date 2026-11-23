#!/usr/bin/env bash
set -euo pipefail

DART_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../dart" && pwd)
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
# Checkouts use a dev build; benchmark and release staging opt into release.
profile=${MOQ_FFI_PROFILE:-debug}
# Share the `testing,openh264` target directory with py while keeping test hooks
# out of the default cdylib. Keep the full caller environment so Cargo and mbx
# caches stay active; sh/dart/check.sh pins the built library via user_defines
# so the Dart build hook never invokes Cargo a second time.
target_root="${CARGO_TARGET_DIR:-$WORKSPACE_DIR/target}"
[[ "$target_root" = /* ]] || target_root="$WORKSPACE_DIR/$target_root"
testing_target_dir="$target_root/ffi-testing"

# The debug build is the one `maturin develop` runs for py, a cdylib alone, so
# after py's build Cargo finds it fresh. Any other crate type would compile
# moq-ffi again, a crate mbx cannot cache.
case "$profile" in
    debug) build=(cargo rustc --locked --package moq-ffi --features "testing,openh264" --lib --crate-type cdylib --target-dir "$testing_target_dir") ;;
    release) build=(cargo build-ffi --release) ;;
    *)
        echo "error: unsupported MOQ_FFI_PROFILE: $profile" >&2
        exit 1
        ;;
esac
messages=$(cd "$WORKSPACE_DIR" && "${build[@]}" --message-format=json-render-diagnostics)

if [[ "$profile" == debug ]]; then
    target_dir=$(cd "$WORKSPACE_DIR" && CARGO_TARGET_DIR="$testing_target_dir" \
        cargo metadata --format-version 1 --no-deps | jq -er .target_directory)
else
    target_dir=$(cd "$WORKSPACE_DIR" && cargo metadata --format-version 1 --no-deps | jq -er .target_directory)
fi
case "$host" in
    *-apple-*) name=libmoq_ffi.dylib ;;
    *-windows-*) name=moq_ffi.dll ;;
    *) name=libmoq_ffi.so ;;
esac
library=$(jq -ers --arg root "$target_dir" --arg target "$host" --arg host "$host" \
    --arg profile "$profile" --arg library "$name" -f "$WORKSPACE_DIR/sh/rs/ffi-artifact.jq" <<<"$messages")

if [[ -n "${MOQ_DART_LIBRARY_OUT:-}" ]]; then
    printf '%s\n' "$library" >"$MOQ_DART_LIBRARY_OUT"
fi

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

# uniffi_bindgen_dart names the record converter after the Rust type
# (`MoqProtocolError`) but the exception variant after the Dart type
# (`MoqProtocolException`). Point the variant at the class that was emitted.
sed -i.bak 's/FfiConverterMoqProtocolException/FfiConverterMoqProtocolError/g' "$OUTPUT_DIR/moq.dart"
rm -f "$OUTPUT_DIR/moq.dart.bak"

dart format --language-version 3.10 "$OUTPUT_DIR/moq.dart" "$OUTPUT_DIR/uniffi_runtime.dart"
