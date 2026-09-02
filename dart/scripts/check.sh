#!/usr/bin/env bash
set -euo pipefail

DART_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ACTION=check

if [[ "${1:-}" == --fix ]]; then
    ACTION=fix
    shift
fi

# `just check` passes the whole changed-file list, so match the scope anywhere in
# it rather than only at the start.
files="${1:-}"
if [[ -n "$files" ]] && ! grep -qE '(^|[[:space:]])(dart/|rs/moq-ffi/)' <<<"$files"; then
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

if [[ "$ACTION" == fix ]]; then
    dart format "$DART_DIR/moq_ffi" "$DART_DIR/moq"
    exit 0
fi

# package.sh injects the real version from the moq-ffi-v* tag. A hardcoded one
# would silently go stale on the next release-plz bump, and the build hook uses
# it to name the native assets it downloads, so it would fetch a library whose
# ABI no longer matches these bindings.
pubspec_version=$(sed -n 's/^version: //p' "$DART_DIR/moq_ffi/pubspec.yaml" | head -1)
hook_version=$(sed -n "s/^const ffiVersion = '\(.*\)';/\1/p" "$DART_DIR/moq_ffi/hook/build.dart" | head -1)
for pinned in "$pubspec_version:moq_ffi/pubspec.yaml" "$hook_version:moq_ffi/hook/build.dart"; do
    if [[ "${pinned%%:*}" != "0.0.0-dev" ]]; then
        echo "error: ${pinned#*:} pins ${pinned%%:*}; commit the 0.0.0-dev sentinel instead" >&2
        exit 1
    fi
done

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
