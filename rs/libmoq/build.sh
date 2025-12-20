#!/usr/bin/env bash
set -euo pipefail

# Build and package libmoq for release.
# Usage: ./build.sh [--target TARGET] [--version VERSION] [--output DIR]
#
# Examples:
#   ./build.sh                                    # Build for host, detect version from Cargo.toml
#   ./build.sh --target aarch64-apple-darwin      # Cross-compile for Apple Silicon

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$RS_DIR/.." && pwd)"

# Defaults
TARGET=""
VERSION=""
OUTPUT_DIR="dist"

# Parse arguments
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
        -h|--help)
            echo "Usage: $0 [--target TARGET] [--version VERSION] [--output DIR]"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

# Detect target if not specified
if [[ -z "$TARGET" ]]; then
    TARGET=$(rustc -vV | grep host | cut -d' ' -f2)
    echo "Detected target: $TARGET"
fi

# Get version from Cargo.toml if not specified
if [[ -z "$VERSION" ]]; then
    VERSION=$(grep '^version' "$SCRIPT_DIR/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
    echo "Detected version: $VERSION"
fi

echo "Building libmoq for $TARGET..."

# Set up cross-compilation for Linux ARM64
if [[ "$TARGET" == "aarch64-unknown-linux-gnu" ]]; then
	export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
fi

cargo build --release --package libmoq --target "$TARGET" --manifest-path "$WORKSPACE_DIR/Cargo.toml"

# Determine paths
TARGET_DIR="$WORKSPACE_DIR/target/$TARGET/release"
NAME="moq-${VERSION}-${TARGET}"
PACKAGE_DIR="$OUTPUT_DIR/$NAME"

echo "Packaging $NAME..."

# Clean and create package directory
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/include" "$PACKAGE_DIR/lib"

# Copy header (generated in target/include/ by build.rs)
HEADER_FILE="$WORKSPACE_DIR/target/include/moq.h"
if [[ -f "$HEADER_FILE" ]]; then
    cp "$HEADER_FILE" "$PACKAGE_DIR/include/"
else
    echo "Error: moq.h not found at $HEADER_FILE" >&2
    exit 1
fi

# Copy static library
case "$TARGET" in
    *-windows-*)
        cp "$TARGET_DIR/moq.lib" "$PACKAGE_DIR/lib/"
        ;;
    *)
        # Unix-like (macOS, Linux, etc.)
        cp "$TARGET_DIR/libmoq.a" "$PACKAGE_DIR/lib/"
        ;;
esac

# Copy pkg-config file (generated in target/ by build.rs, not for Windows)
if [[ "$TARGET" != *"-windows-"* ]]; then
    mkdir -p "$PACKAGE_DIR/lib/pkgconfig"
    cp "$WORKSPACE_DIR/target/moq.pc" "$PACKAGE_DIR/lib/pkgconfig/"
fi

# Generate CMake config files from templates
mkdir -p "$PACKAGE_DIR/lib/cmake/moq"

# Determine library filename
if [[ "$TARGET" == *"-windows-"* ]]; then
    LIB_FILE="moq.lib"
else
    LIB_FILE="libmoq.a"
fi

# Extract major version
MAJOR_VERSION="${VERSION%%.*}"

# Generate moq-config.cmake from template
sed -e "s|@LIB_FILE@|${LIB_FILE}|g" \
    -e "s|@VERSION@|${VERSION}|g" \
    "$SCRIPT_DIR/cmake/moq-config.cmake.in" > "$PACKAGE_DIR/lib/cmake/moq/moq-config.cmake"

# Generate moq-config-version.cmake from template
sed -e "s|@VERSION@|${VERSION}|g" \
    -e "s|@MAJOR_VERSION@|${MAJOR_VERSION}|g" \
    "$SCRIPT_DIR/cmake/moq-config-version.cmake.in" > "$PACKAGE_DIR/lib/cmake/moq/moq-config-version.cmake"

echo "Generated CMake config files from templates"

# Create archive
cd "$OUTPUT_DIR"
if [[ "$TARGET" == *"-windows-"* ]]; then
    ARCHIVE="$NAME.zip"
    if command -v 7z &> /dev/null; then
        7z a "$ARCHIVE" "$NAME"
    elif command -v zip &> /dev/null; then
        zip -r "$ARCHIVE" "$NAME"
    else
        echo "Error: Neither 7z nor zip found" >&2
        exit 1
    fi
else
    ARCHIVE="$NAME.tar.gz"
    tar -czvf "$ARCHIVE" "$NAME"
fi

# Clean up directory, keep archive
rm -rf "$PACKAGE_DIR"

echo ""
echo "Created: $OUTPUT_DIR/$ARCHIVE"
echo "$ARCHIVE"
