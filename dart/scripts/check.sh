#!/usr/bin/env bash
set -euo pipefail

DART_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORKSPACE_DIR=$(cd "$DART_DIR/.." && pwd)
ACTION=check

if [[ "${1:-}" == --fix ]]; then
    ACTION=fix
    shift
fi

files="${1:-}"
if [[ -n "$files" ]] && ! grep -qE '^(dart/|rs/moq-ffi/)' <<<"$files"; then
    exit 0
fi

for tool in cargo dart uniffi_bindgen_dart; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "dart check: skipping, $tool is not installed" >&2
        exit 0
    fi
done

for package in moq_ffi moq; do
    (
        cd "$DART_DIR/$package"
        dart pub get --enforce-lockfile
    )
done

if command -v flutter >/dev/null 2>&1; then
    (
        cd "$DART_DIR/example/flutter"
        flutter pub get --enforce-lockfile
    )
fi

if [[ "$ACTION" == fix ]]; then
    dart format "$DART_DIR/moq_ffi" "$DART_DIR/moq"
    if command -v flutter >/dev/null 2>&1; then
        dart format "$DART_DIR/example/flutter"
    fi
    exit 0
fi

generated=$(mktemp -d)
trap 'rm -rf "$generated"' EXIT
"$DART_DIR/scripts/generate.sh" "$generated"

for file in moq.dart uniffi_runtime.dart; do
    if ! diff -u -B "$DART_DIR/moq_ffi/lib/src/$file" "$generated/$file"; then
        echo "error: Dart bindings are stale; run 'just dart generate'" >&2
        exit 1
    fi
done

dart format --output=none --set-exit-if-changed \
    "$DART_DIR/moq_ffi" "$DART_DIR/moq"
if command -v flutter >/dev/null 2>&1; then
    dart format --output=none --set-exit-if-changed \
        "$DART_DIR/example/flutter"
fi

host=$(rustc -vV | sed -n 's/^host: //p')
case "$host" in
    *-apple-*) library="$WORKSPACE_DIR/target/$host/release/libmoq_ffi.dylib" ;;
    *-windows-*) library="$WORKSPACE_DIR/target/$host/release/moq_ffi.dll" ;;
    *) library="$WORKSPACE_DIR/target/$host/release/libmoq_ffi.so" ;;
esac
export MOQ_DART_FFI_LIB="$library"

publish_dir="$generated/publish"
mkdir -p "$publish_dir"
for package in moq_ffi moq; do
    cp -R "$DART_DIR/$package" "$publish_dir/$package"
    rm -rf "$publish_dir/$package/.dart_tool" "$publish_dir/$package/build"

    (
        cd "$DART_DIR/$package"
        dart analyze
        dart test
    )
done

# Pub rejects dry runs from a dirty Git tree. Validate copies outside the
# checkout so this pre-commit check exercises package contents, not Git state.
for package in moq_ffi moq; do
    (
        cd "$publish_dir/$package"
        dart pub get --enforce-lockfile
        dart pub publish --dry-run
    )
done

if command -v flutter >/dev/null 2>&1; then
    (
        cd "$DART_DIR/example/flutter"
        flutter analyze
    )
else
    echo "dart check: skipping Flutter example, flutter is not installed" >&2
fi
