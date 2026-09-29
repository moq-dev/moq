#!/usr/bin/env bash
set -euo pipefail

# Build the moq C++ package for the host and archive it for release.
# Usage: ./build.sh [--target TARGET] [--output DIR]
#
# The archive holds what `cmake --install` lays out: the moq-ffi staticlib, the wrapper and
# generated headers, the generated source, the CMake package, and moq-cpp.pc. Needs cargo, cmake,
# and uniffi-bindgen-cpp on PATH. The version is cpp/moq/VERSION.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Shrink the release staticlib the way rs/libmoq/build.sh does: thin LTO with one codegen
# unit dead-strips the monomorphizations Rust bakes into a staticlib.
export CARGO_PROFILE_RELEASE_LTO="${CARGO_PROFILE_RELEASE_LTO:-thin}"
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS="${CARGO_PROFILE_RELEASE_CODEGEN_UNITS:-1}"

TARGET=""
OUTPUT_DIR="dist"

while [[ $# -gt 0 ]]; do
    case $1 in
        --target)
            TARGET="$2"
            shift 2
            ;;
        --output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        -h | --help)
            echo "Usage: $0 [--target TARGET] [--output DIR]"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

VERSION=$(tr -d '[:space:]' <"$SCRIPT_DIR/VERSION")
HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')
TARGET="${TARGET:-$HOST_TARGET}"

# CMake builds for the host. A mismatch would silently mislabel the archive.
if [[ "$TARGET" != "$HOST_TARGET" ]]; then
    echo "Error: unsupported cross ($HOST_TARGET -> $TARGET); refusing to mislabel the archive." >&2
    exit 1
fi

NAME="moq-cpp-${VERSION}-${TARGET}"
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
PACKAGE_DIR="$OUTPUT_DIR/$NAME"
BUILD_DIR="$SCRIPT_DIR/build/release"

echo "Packaging $NAME..."
rm -rf "$PACKAGE_DIR"
# lib/, not the lib64/ GNUInstallDirs picks on some hosts, so every archive has one layout.
cmake -S "$SCRIPT_DIR" -B "$BUILD_DIR" -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_LIBDIR=lib
cmake --build "$BUILD_DIR" --config Release
cmake --install "$BUILD_DIR" --config Release --prefix "$PACKAGE_DIR"

# A placeholder the configure step missed would reach a consumer's linker as a literal.
# Comments are skipped: configure_package_config_file names @PACKAGE_INIT@ in one.
if grep -rnE '^[^#]*@[A-Z_]+@' "$PACKAGE_DIR/lib/cmake" "$PACKAGE_DIR/lib/pkgconfig"; then
    echo "Error: unsubstituted placeholder in the package config (see above)" >&2
    exit 1
fi

cd "$OUTPUT_DIR"
if [[ "$TARGET" == *"-windows-"* ]]; then
    ARCHIVE="$NAME.zip"
    if command -v 7z &>/dev/null; then
        7z a "$ARCHIVE" "$NAME"
    else
        zip -r "$ARCHIVE" "$NAME"
    fi
else
    ARCHIVE="$NAME.tar.gz"
    tar -czf "$ARCHIVE" "$NAME"
fi
rm -rf "$NAME"

echo "Created: $OUTPUT_DIR/$ARCHIVE"
