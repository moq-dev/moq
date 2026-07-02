#!/usr/bin/env bash
set -euo pipefail

# Build and package a workspace binary as a Windows .zip for release.
# Usage: ./package-windows.sh --crate CRATE [--bin NAME] [--version VERSION] [--target TARGET] [--output DIR]
#
# --bin overrides the binary/command name when it differs from the crate (e.g.
# the `moq-cli` crate ships its binary as `moq`); it defaults to the crate name.
#
# Runs under Git Bash on a windows-latest runner. Unlike package-binary.sh
# (nix, macOS + Linux), this is a plain cargo build against the MSVC target.
# Produces <output>/<crate>-<version>-<target>.zip with the .exe and licenses
# at the archive root. Keeping the layout flat (no versioned top folder) means
# the winget portable manifest's RelativeFilePath stays the same across
# releases, so wingetcreate can carry it forward unchanged.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$RS_DIR/.." && pwd)"

CRATE=""
BIN=""
VERSION=""
TARGET="x86_64-pc-windows-msvc"
OUTPUT_DIR="dist"

while [[ $# -gt 0 ]]; do
    case $1 in
        --crate | --bin | --version | --target | --output)
            if [[ $# -lt 2 ]]; then
                echo "Error: $1 requires a value" >&2
                exit 1
            fi
            case $1 in
                --crate) CRATE="$2" ;;
                --bin) BIN="$2" ;;
                --version) VERSION="$2" ;;
                --target) TARGET="$2" ;;
                --output) OUTPUT_DIR="$2" ;;
            esac
            shift 2
            ;;
        -h | --help)
            echo "Usage: $0 --crate CRATE [--bin NAME] [--version VERSION] [--target TARGET] [--output DIR]"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

if [[ -z "$CRATE" ]]; then
    echo "Error: --crate is required" >&2
    exit 1
fi

# The binary/command name defaults to the crate name.
BIN="${BIN:-$CRATE}"

CRATE_DIR="$RS_DIR/$CRATE"
if [[ ! -f "$CRATE_DIR/Cargo.toml" ]]; then
    echo "Error: no Cargo.toml found at $CRATE_DIR" >&2
    exit 1
fi

if [[ -z "$VERSION" ]]; then
    VERSION=$(grep '^version' "$CRATE_DIR/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
    if [[ -z "$VERSION" ]]; then
        echo "Error: could not detect version from $CRATE_DIR/Cargo.toml" >&2
        exit 1
    fi
    echo "Detected version: $VERSION"
fi

echo "Building $CRATE for $TARGET..."
cargo build --release --target "$TARGET" -p "$CRATE"

# The command name is the crate's `[[bin]]` name (usually the crate name; the
# `moq-cli` crate ships as `moq`), matching `cargo install` and the other packages.
BIN_FILE="$WORKSPACE_DIR/target/$TARGET/release/$BIN.exe"
if [[ ! -f "$BIN_FILE" ]]; then
    echo "Error: no binary found at $BIN_FILE" >&2
    ls "$WORKSPACE_DIR/target/$TARGET/release" >&2 || true
    exit 1
fi

# Smoke test: a release we can't even --help is not worth shipping.
"$BIN_FILE" --help >/dev/null

NAME="$CRATE-$VERSION-$TARGET"
STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT

cp "$BIN_FILE" "$STAGING/$BIN.exe"
cp "$WORKSPACE_DIR/LICENSE-MIT" "$STAGING/"
cp "$WORKSPACE_DIR/LICENSE-APACHE" "$STAGING/"
if [[ -f "$CRATE_DIR/README.md" ]]; then
    cp "$CRATE_DIR/README.md" "$STAGING/"
fi

mkdir -p "$OUTPUT_DIR"
ZIP_ABS="$(cd "$OUTPUT_DIR" && pwd)/$NAME.zip"
rm -f "$ZIP_ABS"

# 7z ships on the windows-latest runner. Zip from inside the staging dir so
# the entries land at the archive root rather than under a temp path.
(cd "$STAGING" && 7z a -tzip -mx=9 "$ZIP_ABS" ./* >/dev/null)

echo ""
echo "Created: $OUTPUT_DIR/$NAME.zip"
echo "$NAME.zip"
