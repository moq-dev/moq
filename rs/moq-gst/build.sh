#!/usr/bin/env bash
set -euo pipefail

# Build and package the moq-gst GStreamer plugin for release.
# Usage: ./build.sh [--target TARGET] [--version VERSION] [--output DIR]
#
# Builds via `nix build .#moq-gst` against the flake-pinned GStreamer so
# artifacts are reproducible, then runs scrub.sh on a copy of the
# produced shared library to rewrite nix-store paths for the user's
# system GStreamer, and smoke.sh to confirm the result actually loads.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$RS_DIR/.." && pwd)"

TARGET=""
VERSION=""
OUTPUT_DIR="dist"

while [[ $# -gt 0 ]]; do
    case $1 in
        --target)
            TARGET="$2"
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
        -h | --help)
            echo "Usage: $0 [--target TARGET] [--version VERSION] [--output DIR]"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

if [[ -z "$VERSION" ]]; then
    VERSION=$(grep '^version' "$SCRIPT_DIR/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
    echo "Detected version: $VERSION"
fi

if [[ -z "$TARGET" ]]; then
    TARGET=$(rustc -vV | grep host | cut -d' ' -f2)
    echo "Detected target: $TARGET"
fi

echo "Building moq-gst for $TARGET via nix..."

BUILD_TMP="$(mktemp -d)"
trap 'rm -rf "$BUILD_TMP"' EXIT
RESULT_LINK="$BUILD_TMP/result"
nix build "$WORKSPACE_DIR#moq-gst" --out-link "$RESULT_LINK"

# Locate the produced shared library (extension differs by platform).
# The flake places it in lib/gstreamer-1.0/ so gst_all_1.gstreamer's
# setup-hook auto-discovers it in `nix shell`.
LIB_FILE=""
for candidate in libgstmoq.so libgstmoq.dylib; do
    if [[ -f "$RESULT_LINK/lib/gstreamer-1.0/$candidate" ]]; then
        LIB_FILE="$candidate"
        break
    fi
done
if [[ -z "$LIB_FILE" ]]; then
    echo "Error: no libgstmoq.{so,dylib} found in $RESULT_LINK/lib/gstreamer-1.0/" >&2
    exit 1
fi

NAME="moq-gst-${VERSION}-${TARGET}"
PACKAGE_DIR="$OUTPUT_DIR/$NAME"

echo "Packaging $NAME..."
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/lib/gstreamer-1.0"

# Dereference the nix-store symlink and drop perms so the file is writable
# enough to archive cleanly. Same layout as the flake output and as what
# .deb / .rpm install to (/usr/lib/<triple>/gstreamer-1.0/), so users can
# `cp lib/gstreamer-1.0/* /path/to/their/gstreamer-1.0/` or point
# GST_PLUGIN_PATH_1_0 at the tarball's lib/gstreamer-1.0 directly.
cp -L "$RESULT_LINK/lib/gstreamer-1.0/$LIB_FILE" "$PACKAGE_DIR/lib/gstreamer-1.0/$LIB_FILE"
chmod 0644 "$PACKAGE_DIR/lib/gstreamer-1.0/$LIB_FILE"

cp "$SCRIPT_DIR/README.md" "$PACKAGE_DIR/"
cp "$WORKSPACE_DIR/LICENSE-MIT" "$PACKAGE_DIR/"
cp "$WORKSPACE_DIR/LICENSE-APACHE" "$PACKAGE_DIR/"

"$SCRIPT_DIR/scrub.sh" "$PACKAGE_DIR/lib/gstreamer-1.0/$LIB_FILE"
"$SCRIPT_DIR/smoke.sh" "$PACKAGE_DIR/lib/gstreamer-1.0"

cd "$OUTPUT_DIR"
ARCHIVE="$NAME.tar.gz"
tar -czvf "$ARCHIVE" "$NAME"
rm -rf "$NAME"

echo ""
echo "Created: $OUTPUT_DIR/$ARCHIVE"
echo "$ARCHIVE"
