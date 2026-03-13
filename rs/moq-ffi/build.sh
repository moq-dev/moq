#!/usr/bin/env bash
set -euo pipefail

# Build and package moq-ffi native libraries for release.
# Usage: ./build.sh [--target TARGET] [--version VERSION] [--output DIR] [--bindings-only]
#
# Examples:
#   ./build.sh                                    # Build for host, detect version from Cargo.toml
#   ./build.sh --target aarch64-apple-darwin      # Cross-compile for Apple Silicon
#   ./build.sh --target aarch64-linux-android     # Cross-compile for Android (requires cargo-ndk)
#   ./build.sh --bindings-only --output dist      # Build for host and generate bindings only

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$RS_DIR/.." && pwd)"

# Resolve cargo target directory (respects CARGO_TARGET_DIR, .cargo/config, etc.)
TARGET_BASE_DIR=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE_DIR/Cargo.toml" --no-deps 2>/dev/null \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' \
    || echo "$WORKSPACE_DIR/target")

# Defaults
TARGET=""
VERSION=""
OUTPUT_DIR="dist"
BINDINGS_ONLY=false

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
        --bindings-only)
            BINDINGS_ONLY=true
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [--target TARGET] [--version VERSION] [--output DIR] [--bindings-only]"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

# Get version from Cargo.toml if not specified
if [[ -z "$VERSION" ]]; then
    VERSION=$(grep '^version' "$SCRIPT_DIR/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
    echo "Detected version: $VERSION"
fi

# Detect host target
HOST_TARGET=$(rustc -vV | grep host | cut -d' ' -f2)

# Detect target if not specified
if [[ -z "$TARGET" ]]; then
    TARGET="$HOST_TARGET"
    echo "Detected target: $TARGET"
fi

# Check if target is an Android target
is_android() {
    [[ "$1" == *"-android"* || "$1" == *"-androideabi"* ]]
}

# Check if target is an iOS target
is_ios() {
    [[ "$1" == *"-apple-ios"* ]]
}

# Check if target can run on the host (for binding generation)
can_run_on_host() {
    local target="$1"
    # Universal darwin can run on host if we're on macOS
    if [[ "$target" == "universal-apple-darwin" && "$(uname)" == "Darwin" ]]; then
        return 0
    fi
    [[ "$target" == "$HOST_TARGET" ]]
}

# Build the library for a single target
build_target() {
    local target="$1"
    echo "Building moq-ffi for $target..."

    if is_android "$target"; then
        # Android targets use cargo-ndk
        cargo ndk --target "$target" --platform 24 -- \
            build --release --package moq-ffi --manifest-path "$WORKSPACE_DIR/Cargo.toml"
    else
        # Set up cross-compilation for Linux ARM64
        if [[ "$target" == "aarch64-unknown-linux-gnu" ]]; then
            export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
        fi

        cargo build --release --package moq-ffi --target "$target" --manifest-path "$WORKSPACE_DIR/Cargo.toml"
    fi
}

# Find the cdylib for binding generation
find_cdylib() {
    local target="$1"
    local target_dir="$TARGET_BASE_DIR/$target/release"

    if [[ "$target" == *"-apple-"* ]]; then
        echo "$target_dir/libmoq_ffi.dylib"
    elif [[ "$target" == *"-windows-"* ]]; then
        echo "$target_dir/moq_ffi.dll"
    else
        echo "$target_dir/libmoq_ffi.so"
    fi
}

# Generate language bindings
generate_bindings() {
    local lib_path="$1"
    echo "Generating bindings from $lib_path..."

    for lang in kotlin swift python; do
        echo "  Generating $lang bindings..."
        cargo run --release --bin uniffi-bindgen --manifest-path "$WORKSPACE_DIR/Cargo.toml" -- \
            generate --library "$lib_path" \
            --language "$lang" --out-dir "$OUTPUT_DIR/bindings/$lang"
    done

    # Package each language binding
    for lang in kotlin swift python; do
        local archive="moq-ffi-${VERSION}-${lang}.tar.gz"
        tar -czvf "$OUTPUT_DIR/$archive" -C "$OUTPUT_DIR/bindings" "$lang"
        echo "Created: $OUTPUT_DIR/$archive"
    done

    # Clean up unarchived bindings
    rm -rf "$OUTPUT_DIR/bindings"
}

mkdir -p "$OUTPUT_DIR"

# --- Bindings-only mode ---
if [[ "$BINDINGS_ONLY" == true ]]; then
    build_target "$HOST_TARGET"
    cdylib=$(find_cdylib "$HOST_TARGET")
    if [[ ! -f "$cdylib" ]]; then
        echo "Error: cdylib not found at $cdylib" >&2
        exit 1
    fi
    generate_bindings "$cdylib"
    echo "Done (bindings only)."
    exit 0
fi

# --- Full build mode ---

if [[ "$TARGET" == "universal-apple-darwin" ]]; then
    if [[ "$(uname)" != "Darwin" ]]; then
        echo "Error: Universal builds are only supported on macOS" >&2
        exit 1
    fi

    build_target "x86_64-apple-darwin"
    build_target "aarch64-apple-darwin"

    LIB_X86_STATIC="$TARGET_BASE_DIR/x86_64-apple-darwin/release/libmoq_ffi.a"
    LIB_ARM64_STATIC="$TARGET_BASE_DIR/aarch64-apple-darwin/release/libmoq_ffi.a"
    LIB_X86_DYLIB="$TARGET_BASE_DIR/x86_64-apple-darwin/release/libmoq_ffi.dylib"
    LIB_ARM64_DYLIB="$TARGET_BASE_DIR/aarch64-apple-darwin/release/libmoq_ffi.dylib"
else
    build_target "$TARGET"
fi

# Package native libraries
NAME="moq-ffi-${VERSION}-${TARGET}"
PACKAGE_DIR="$OUTPUT_DIR/$NAME"

echo "Packaging $NAME..."

rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/lib"

if [[ "$TARGET" == "universal-apple-darwin" ]]; then
    echo "Creating universal binaries..."
    lipo -create "$LIB_X86_STATIC" "$LIB_ARM64_STATIC" -output "$PACKAGE_DIR/lib/libmoq_ffi.a"
    lipo -create "$LIB_X86_DYLIB" "$LIB_ARM64_DYLIB" -output "$PACKAGE_DIR/lib/libmoq_ffi.dylib"

elif [[ "$TARGET" == *"-windows-"* ]]; then
    TARGET_DIR="$TARGET_BASE_DIR/$TARGET/release"
    cp "$TARGET_DIR/moq_ffi.dll" "$PACKAGE_DIR/lib/"
    cp "$TARGET_DIR/moq_ffi.dll.lib" "$PACKAGE_DIR/lib/" 2>/dev/null || true
    cp "$TARGET_DIR/moq_ffi.lib" "$PACKAGE_DIR/lib/" 2>/dev/null || true

elif is_ios "$TARGET"; then
    # iOS: staticlib only (no dylib)
    TARGET_DIR="$TARGET_BASE_DIR/$TARGET/release"
    cp "$TARGET_DIR/libmoq_ffi.a" "$PACKAGE_DIR/lib/"

elif is_android "$TARGET"; then
    # Android: shared library only
    TARGET_DIR="$TARGET_BASE_DIR/$TARGET/release"
    cp "$TARGET_DIR/libmoq_ffi.so" "$PACKAGE_DIR/lib/"

elif [[ "$TARGET" == *"-apple-"* ]]; then
    TARGET_DIR="$TARGET_BASE_DIR/$TARGET/release"
    cp "$TARGET_DIR/libmoq_ffi.a" "$PACKAGE_DIR/lib/"
    cp "$TARGET_DIR/libmoq_ffi.dylib" "$PACKAGE_DIR/lib/"

else
    TARGET_DIR="$TARGET_BASE_DIR/$TARGET/release"
    cp "$TARGET_DIR/libmoq_ffi.a" "$PACKAGE_DIR/lib/"
    cp "$TARGET_DIR/libmoq_ffi.so" "$PACKAGE_DIR/lib/"
fi

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
rm -rf "$NAME"

echo ""
echo "Created: $OUTPUT_DIR/$ARCHIVE"

# Generate bindings if we can run the library on this host
cd "$WORKSPACE_DIR"
if can_run_on_host "$TARGET"; then
    # For universal builds, use the dylib matching the host arch
    if [[ "$TARGET" == "universal-apple-darwin" ]]; then
        host_arch=$(uname -m)
        case "$host_arch" in
            arm64|aarch64) cdylib=$(find_cdylib "aarch64-apple-darwin") ;;
            x86_64)        cdylib=$(find_cdylib "x86_64-apple-darwin") ;;
            *)             echo "Warning: unknown host arch $host_arch, skipping bindings"; cdylib="" ;;
        esac
    else
        cdylib=$(find_cdylib "$TARGET")
    fi

    if [[ -f "$cdylib" ]]; then
        generate_bindings "$cdylib"
    else
        echo "Warning: cdylib not found at $cdylib, skipping binding generation"
    fi
fi

echo "Done."
