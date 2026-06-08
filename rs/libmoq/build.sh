#!/usr/bin/env bash
set -euo pipefail

# Build and package libmoq for release.
# Usage: ./build.sh [--target TARGET] [--version VERSION] [--output DIR]
#
# Examples:
#   ./build.sh                                    # Build for host, detect version from Cargo.toml
#   ./build.sh --target aarch64-apple-darwin      # Native build (run on matching runner)
#
# On Linux and macOS this builds via `nix build .#libmoq` for reproducibility.
# Windows targets fall back to a direct cargo build because Nix isn't
# practical to install on the Windows GitHub runner image.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$RS_DIR/.." && pwd)"

# Shrink the release staticlib. libmoq.a carries the same heavy dep tree as
# moq-ffi, so an unstripped build is ~75 MB+. libmoq ships as a release
# tarball (not a git mirror, so no hard 100 MB limit like moq-go), but thin
# LTO with a single codegen unit dead-strips the unused monomorphizations Rust
# bakes into a staticlib, halving the artifact with no source or ABI changes.
# This covers the Windows cargo path below; the nix/crane path (Linux/macOS)
# sets the same vars in nix/overlay.nix. Scoped here so a plain
# `cargo build --release` stays fast; a caller can still override.
export CARGO_PROFILE_RELEASE_LTO="${CARGO_PROFILE_RELEASE_LTO:-thin}"
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS="${CARGO_PROFILE_RELEASE_CODEGEN_UNITS:-1}"

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

NAME="moq-${VERSION}-${TARGET}"
PACKAGE_DIR="$OUTPUT_DIR/$NAME"

echo "Packaging $NAME..."
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/include" "$PACKAGE_DIR/lib"

if [[ "$TARGET" == *"-windows-"* ]]; then
    echo "Building libmoq for $TARGET via cargo (Windows path)..."
    cargo build --release --package libmoq --target "$TARGET" --manifest-path "$WORKSPACE_DIR/Cargo.toml"

    TARGET_DIR="$WORKSPACE_DIR/target/$TARGET/release"
    LIB_FILE="moq.lib"
    cp "$TARGET_DIR/$LIB_FILE" "$PACKAGE_DIR/lib/"
    cp "$WORKSPACE_DIR/target/$TARGET/include/moq.h" "$PACKAGE_DIR/include/"

    # Generate CMake config files from templates (no pkg-config on Windows).
    mkdir -p "$PACKAGE_DIR/lib/cmake/moq"
    MAJOR_VERSION="${VERSION%%.*}"
    sed -e "s|@LIB_FILE@|${LIB_FILE}|g" \
        -e "s|@VERSION@|${VERSION}|g" \
        "$SCRIPT_DIR/cmake/moq-config.cmake.in" >"$PACKAGE_DIR/lib/cmake/moq/moq-config.cmake"
    sed -e "s|@VERSION@|${VERSION}|g" \
        -e "s|@MAJOR_VERSION@|${MAJOR_VERSION}|g" \
        "$SCRIPT_DIR/cmake/moq-config-version.cmake.in" >"$PACKAGE_DIR/lib/cmake/moq/moq-config-version.cmake"
else
    # Native builds use the bare flake output; the one supported cross is the
    # Intel mac release built on an Apple Silicon runner (the Determinate Nix
    # installer dropped Intel macOS). The flake exposes a per-target output for
    # it; Apple's clang cross-compiles natively. libmoq.a needs no execution,
    # so unlike the binary tarballs there's no Rosetta smoke test.
    HOST_TARGET=$(rustc -vV | grep host | cut -d' ' -f2)
    NIX_ATTR="libmoq"
    if [[ "$TARGET" != "$HOST_TARGET" ]]; then
        if [[ "$HOST_TARGET" == "aarch64-apple-darwin" && "$TARGET" == "x86_64-apple-darwin" ]]; then
            NIX_ATTR="libmoq-$TARGET"
        else
            echo "Error: unsupported cross ($HOST_TARGET -> $TARGET)." >&2
            echo "Only aarch64-apple-darwin -> x86_64-apple-darwin is wired up." >&2
            exit 1
        fi
    fi

    echo "Building libmoq for $TARGET via nix (output: $NIX_ATTR)..."
    BUILD_TMP="$(mktemp -d)"
    trap 'rm -rf "$BUILD_TMP"' EXIT
    RESULT_LINK="$BUILD_TMP/result"
    nix build "$WORKSPACE_DIR#$NIX_ATTR" --out-link "$RESULT_LINK"

    # The derivation lays out everything under $out/{lib,include}; copy it
    # verbatim so the release tarball matches what nix produced.
    cp -RL "$RESULT_LINK/lib/." "$PACKAGE_DIR/lib/"
    cp -RL "$RESULT_LINK/include/." "$PACKAGE_DIR/include/"
    chmod -R u+w "$PACKAGE_DIR"
fi

# Create archive
cd "$OUTPUT_DIR"
if [[ "$TARGET" == *"-windows-"* ]]; then
    ARCHIVE="$NAME.zip"
    if command -v 7z &>/dev/null; then
        7z a "$ARCHIVE" "$NAME"
    elif command -v zip &>/dev/null; then
        zip -r "$ARCHIVE" "$NAME"
    else
        echo "Error: Neither 7z nor zip found" >&2
        exit 1
    fi
else
    ARCHIVE="$NAME.tar.gz"
    tar -czvf "$ARCHIVE" "$NAME"
fi

rm -rf "$NAME"

echo ""
echo "Created: $OUTPUT_DIR/$ARCHIVE"
echo "$ARCHIVE"
