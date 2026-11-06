#!/usr/bin/env bash
set -euo pipefail

# Build and package the obs-moq plugin for release.
# Usage: ./build.sh [--target TARGET] [--version VERSION] [--output DIR]
#                   [--moq-release VERSION | --moq-archive FILE]
#
# The required toolchain must already be on PATH; this script only drives
# CMake. Per platform:
#   Linux   - run inside `nix develop` (provides cmake/ninja/obs-studio/qt6/ffmpeg)
#   macOS   - full Xcode, run OUTSIDE nix (libobs/Qt6/ffmpeg all come from the
#             obs-deps bundle downloaded by buildspec.json at configure time)
#   Windows - Visual Studio 2022; run from Git Bash with cmake on PATH
#             (libobs/Qt6 downloaded by buildspec.json)
#
# Produces $OUTPUT_DIR/obs-moq-$VERSION-$TARGET.{tar.gz,zip}. The archive is
# unsigned; macOS Gatekeeper / Windows SmartScreen will warn on first load.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TARGET=""
VERSION=""
OUTPUT_DIR="dist"
MOQ_RELEASE=""
MOQ_ARCHIVE_FILE=""

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
        --moq-archive)
            MOQ_ARCHIVE_FILE="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
            [[ -f "$MOQ_ARCHIVE_FILE" ]] || {
                echo "Archive not found: $MOQ_ARCHIVE_FILE" >&2
                exit 1
            }
            shift 2
            ;;
        --moq-release)
            # Link a published moq-cpp release of this version instead of
            # building cpp/moq from source. CMake fetches the matching
            # moq-cpp-<version>-<target> archive from the GitHub release. The
            # plugin keeps its own version. Used by CI on a cpp-v* tag.
            MOQ_RELEASE="$2"
            shift 2
            ;;
        -h | --help)
            echo "Usage: $0 [--target TARGET] [--version VERSION] [--output DIR] [--moq-release VERSION | --moq-archive FILE]"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

if [[ -n "$MOQ_RELEASE" && -n "$MOQ_ARCHIVE_FILE" ]]; then
    echo "Choose --moq-release or --moq-archive" >&2
    exit 1
fi
if [[ -n "$MOQ_ARCHIVE_FILE" ]]; then
    archive_name=$(basename "$MOQ_ARCHIVE_FILE")
    if [[ ! "$archive_name" =~ ^moq-cpp-([0-9]+\.[0-9]+\.[0-9]+)- ]]; then
        echo "Malformed moq-cpp archive name: $archive_name" >&2
        exit 1
    fi
    MOQ_RELEASE="${BASH_REMATCH[1]}"
fi

if [[ -z "$TARGET" ]]; then
    TARGET=$(cc -dumpmachine 2>/dev/null || echo unknown)
    echo "Detected target: $TARGET"
fi

# Default to the plugin's own version.
if [[ -z "$VERSION" ]]; then
    VERSION=$(tr -d '[:space:]' <"$SCRIPT_DIR/VERSION")
    echo "Detected version: $VERSION"
fi

# Map the target triple to a CMake preset and build tree.
case "$TARGET" in
    *-linux-*)
        PRESET="ubuntu-x86_64"
        BUILD_DIR="$SCRIPT_DIR/build_x86_64"
        KIND="unix"
        ;;
    *-apple-darwin)
        PRESET="macos"
        BUILD_DIR="$SCRIPT_DIR/build_macos"
        KIND="macos"
        ;;
    *-windows-*)
        PRESET="windows-x64"
        BUILD_DIR="$SCRIPT_DIR/build_x64"
        KIND="windows"
        ;;
    *)
        echo "Unsupported target: $TARGET" >&2
        exit 1
        ;;
esac

# Resolve the output dir to an absolute path before we cd into the plugin
# directory (cmake --preset reads CMakePresets.json from the current dir).
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
cd "$SCRIPT_DIR"

echo "Building obs-moq $VERSION for $TARGET (preset: $PRESET)..."
# Clear cached archive/source choices when changing package inputs.
CONFIGURE_ARGS=("-DMOQ_LOCAL=$SCRIPT_DIR/../.." -DMOQ_VERSION= "-DMOQ_ARCHIVE_FILE=$MOQ_ARCHIVE_FILE")
# Stamp the plugin's compiled-in version (project version, macOS Info.plist,
# Windows resource) to match what we're building, not buildspec.json's 0.0.1.
if [[ -n "$VERSION" ]]; then
    CONFIGURE_ARGS+=("-DPLUGIN_VERSION_OVERRIDE=$VERSION")
fi
if [[ -n "$MOQ_RELEASE" ]]; then
    # Empty MOQ_LOCAL selects an archive; MOQ_VERSION and
    # MOQ_TARGET steer it at this target's archive. MOQ_ARCHIVE is correct per
    # preset already.
    echo "Linking moq-cpp v$MOQ_RELEASE ($TARGET)${MOQ_ARCHIVE_FILE:+ from $MOQ_ARCHIVE_FILE}"
    CONFIGURE_ARGS+=(-DMOQ_LOCAL= "-DMOQ_VERSION=$MOQ_RELEASE" "-DMOQ_TARGET=$TARGET")
fi
cmake --preset "$PRESET" ${CONFIGURE_ARGS[@]+"${CONFIGURE_ARGS[@]}"}
cmake --build --preset "$PRESET"

NAME="obs-moq-${VERSION}-${TARGET}"
STAGE="$OUTPUT_DIR/$NAME"
rm -rf "$STAGE"
mkdir -p "$OUTPUT_DIR"

if [[ "$KIND" == "macos" ]]; then
    # Self-contained loadable bundle; drop into the OBS plugins directory.
    PLUGIN=$(find "$BUILD_DIR" -name 'obs-moq.plugin' -maxdepth 4 -print -quit)
    [[ -n "$PLUGIN" ]] || {
        echo "obs-moq.plugin not found under $BUILD_DIR" >&2
        exit 1
    }
    mkdir -p "$STAGE"
    cp -R "$PLUGIN" "$STAGE/"
else
    # OBS portable-plugin layout: extract into your OBS plugins directory.
    LIB=$(find "$BUILD_DIR" \( -name 'obs-moq.so' -o -name 'obs-moq.dll' \) -print -quit)
    [[ -n "$LIB" ]] || {
        echo "obs-moq.{so,dll} not found under $BUILD_DIR" >&2
        exit 1
    }
    mkdir -p "$STAGE/obs-moq/bin/64bit"
    cp "$LIB" "$STAGE/obs-moq/bin/64bit/"
    cp -R "$SCRIPT_DIR/data" "$STAGE/obs-moq/"
fi

cp "$SCRIPT_DIR/LICENSE" "$STAGE/"
cp "$SCRIPT_DIR/README.md" "$STAGE/"

# Archive with CMake's tar so we don't depend on zip/gtar being present
# (notably on the Windows runner). tar.gz on unix, zip on macOS/Windows.
(
    cd "$OUTPUT_DIR"
    if [[ "$KIND" == "unix" ]]; then
        ARCHIVE="$NAME.tar.gz"
        cmake -E tar czf "$ARCHIVE" "$NAME"
    else
        ARCHIVE="$NAME.zip"
        cmake -E tar cf "$ARCHIVE" --format=zip "$NAME"
    fi
    rm -rf "$NAME"
    echo "Created: $OUTPUT_DIR/$ARCHIVE"
)
