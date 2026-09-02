#!/usr/bin/env bash
set -euo pipefail

DART_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PACKAGE="${1:?usage: publish.sh <moq|moq_ffi> <version>}"
VERSION="${2:?usage: publish.sh <moq|moq_ffi> <version>}"
OUTPUT=$(mktemp -d)
trap 'rm -rf "$OUTPUT"' EXIT

package_dir=$(
    "$DART_DIR/scripts/package.sh" \
        --package "$PACKAGE" \
        --version "$VERSION" \
        --output "$OUTPUT"
)

cd "$package_dir"
# moq_ffi keeps its committed lock through packaging, so resolve from it.
# moq loses its lock with the path override (see package.sh).
if [[ -f pubspec.lock ]]; then
    dart pub get --enforce-lockfile
else
    dart pub get
fi
dart pub publish --force
