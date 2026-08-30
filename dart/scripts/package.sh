#!/usr/bin/env bash
set -euo pipefail

DART_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PACKAGE=""
VERSION=""
OUTPUT_DIR="dist"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --package)
            PACKAGE="$2"
            shift 2
            ;;
        --version)
            VERSION="$2"
            shift 2
            ;;
        --output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        *)
            echo "error: unknown option $1" >&2
            exit 1
            ;;
    esac
done

[[ "$PACKAGE" == moq || "$PACKAGE" == moq_ffi ]] || {
    echo "error: --package must be moq or moq_ffi" >&2
    exit 1
}
[[ -n "$VERSION" ]] || {
    echo "error: --version is required" >&2
    exit 1
}

destination="$OUTPUT_DIR/$PACKAGE-$VERSION"
rm -rf "$destination"
mkdir -p "$destination"
cp -R "$DART_DIR/$PACKAGE"/. "$destination/"
rm -rf "$destination/.dart_tool" "$destination/build"

sed -i.bak "s/^version: .*/version: $VERSION/" "$destination/pubspec.yaml"
rm -f "$destination/pubspec.yaml.bak"

if [[ "$PACKAGE" == moq_ffi ]]; then
    sed -i.bak "s/^const ffiVersion = .*/const ffiVersion = '$VERSION';/" \
        "$destination/hook/build.dart"
    rm -f "$destination/hook/build.dart.bak"
else
    awk '
        /^dependency_overrides:/ { skip = 1; next }
        skip && /^[^[:space:]]/ { skip = 0 }
        !skip { print }
    ' "$destination/pubspec.yaml" >"$destination/pubspec.yaml.tmp"
    mv "$destination/pubspec.yaml.tmp" "$destination/pubspec.yaml"
fi

echo "$destination"
