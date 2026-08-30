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
dart pub get
dart pub publish --force
