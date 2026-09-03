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
    # moq_ffi tracks the crate, so its committed sources carry a 0.0.0-dev
    # sentinel and the tag supplies the real version here. The hook names the
    # native assets it downloads after it, and pub warns when the changelog
    # does not mention the version being published.
    sed -i.bak "s/^const ffiVersion = .*/const ffiVersion = '$VERSION';/" \
        "$destination/hook/build.dart"
    rm -f "$destination/hook/build.dart.bak"

    sed -i.bak "s/^## 0\.0\.0-dev$/## $VERSION/" "$destination/CHANGELOG.md"
    rm -f "$destination/CHANGELOG.md.bak"
else
    # The override points moq_ffi at ../moq_ffi, which only exists in this
    # checkout. Dropping it also invalidates the committed lock, which resolved
    # moq_ffi by path, so remove that too rather than ship a lie: the published
    # graph is instead pinned by the caret constraint in pubspec.yaml.
    awk '
        /^dependency_overrides:/ { skip = 1; next }
        skip && /^[^[:space:]]/ { skip = 0 }
        !skip { print }
    ' "$destination/pubspec.yaml" >"$destination/pubspec.yaml.tmp"
    mv "$destination/pubspec.yaml.tmp" "$destination/pubspec.yaml"
    rm -f "$destination/pubspec.lock"
fi

echo "$destination"
