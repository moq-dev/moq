#!/usr/bin/env bash
set -euo pipefail

# Build and package a workspace binary for release.
# Usage: ./package-binary.sh --crate CRATE [--bin NAME] [--binary PATH] [--bare]
#   [--target TARGET] [--version VERSION] [--output DIR]
#
# --bin overrides the binary/command name when it differs from the crate (e.g.
# the `moq-cli` crate ships its binary as `moq`); it defaults to the crate name.
#
# Builds via `nix build .#<crate>` against the flake-pinned toolchain unless
# --binary supplies an existing build. Produces
# <output>/<crate>-v<version>-<target>.tar.gz, named after the release tag so
# a URL rewritten from one tag to the next still resolves; the layout matches
# the Homebrew tap templates in .github/homebrew/Formula/. With --bare, also
# produces an extensionless executable beside the archive.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$RS_DIR/.." && pwd)"

CRATE=""
BIN=""
BINARY=""
BARE=false
TARGET=""
VERSION=""
OUTPUT_DIR="dist"

while [[ $# -gt 0 ]]; do
    case $1 in
        --crate | --bin | --binary | --target | --version | --output)
            if [[ $# -lt 2 ]]; then
                echo "Error: $1 requires a value" >&2
                exit 1
            fi
            case $1 in
                --crate) CRATE="$2" ;;
                --bin) BIN="$2" ;;
                --binary) BINARY="$2" ;;
                --target) TARGET="$2" ;;
                --version) VERSION="$2" ;;
                --output) OUTPUT_DIR="$2" ;;
            esac
            shift 2
            ;;
        --bare)
            BARE=true
            shift
            ;;
        -h | --help)
            echo "Usage: $0 --crate CRATE [--bin NAME] [--binary PATH] [--bare] [--target TARGET] [--version VERSION] [--output DIR]"
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
    echo "Detected version: $VERSION"
fi

if [[ -z "$TARGET" ]]; then
    TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
    echo "Detected target: $TARGET"
fi

# Release builds must match the host. A mismatch would silently mislabel the
# archive, so reject it before building.
HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
if [[ "$TARGET" != "$HOST_TARGET" ]]; then
    echo "Error: unsupported cross ($HOST_TARGET -> $TARGET); refusing to mislabel the archive." >&2
    exit 1
fi

BUILD_TMP="$(mktemp -d)"
trap 'rm -rf "$BUILD_TMP"' EXIT
if [[ -n "$BINARY" ]]; then
    BIN_FILE="$BINARY"
    echo "Packaging prebuilt $CRATE binary for $TARGET..."
else
    echo "Building $CRATE for $TARGET via nix (output: $CRATE)..."
    RESULT_LINK="$BUILD_TMP/result"
    nix build "$WORKSPACE_DIR#$CRATE" --out-link "$RESULT_LINK"

    # Crane installs to result/bin/<binary>. The binary name is usually the
    # crate name; the `moq-cli` crate ships as `moq`.
    BIN_FILE="$RESULT_LINK/bin/$BIN"
fi

if [[ ! -f "$BIN_FILE" ]]; then
    echo "Error: no binary found at $BIN_FILE" >&2
    if [[ -n "${RESULT_LINK:-}" ]]; then
        echo "Contents of $RESULT_LINK/bin:" >&2
        ls "$RESULT_LINK/bin" >&2 || true
    fi
    exit 1
fi

NAME="$CRATE-v$VERSION-$TARGET"
PACKAGE_DIR="$BUILD_TMP/$NAME"

echo "Packaging $NAME..."
mkdir -p "$PACKAGE_DIR/bin"

# Dereference the nix-store symlink and drop perms so the file is writable
# enough to archive cleanly.
cp -L "$BIN_FILE" "$PACKAGE_DIR/bin/$BIN"
chmod 0755 "$PACKAGE_DIR/bin/$BIN"

# On macOS the nix toolchain links the nix-store copy of libiconv, whose
# absolute path doesn't exist on a user's Mac, so dyld aborts at startup
# (the brew channel's "Library not loaded: /nix/store/.../libiconv.2.dylib").
# Rewrite the leaked paths to load from the system dyld shared cache (via
# /usr/lib), like a plain `cargo build` does. scrub-macho.sh also asserts no
# /nix path survives, so this can't silently ship again.
if [[ "$(uname)" == "Darwin" ]]; then
    bin="$PACKAGE_DIR/bin/$BIN"
    "$SCRIPT_DIR/scrub-macho.sh" "$bin"

    # Prove dyld actually loads the scrubbed binary. --help triggers dyld's
    # dependency load (the exact step that aborted on a clean Mac) before clap
    # exits 0. Native for the host arch; the x86_64 cross build runs under
    # Rosetta 2, which the workflow installs before invoking this script.
    if ! "$bin" --help >/dev/null 2>&1; then
        echo "Error: scrubbed $bin failed to launch (dyld load failure, or missing Rosetta for a cross build?)." >&2
        otool -L "$bin" >&2
        exit 1
    fi
fi

if [[ -f "$CRATE_DIR/README.md" ]]; then
    cp "$CRATE_DIR/README.md" "$PACKAGE_DIR/"
fi
cp "$WORKSPACE_DIR/LICENSE-MIT" "$PACKAGE_DIR/"
cp "$WORKSPACE_DIR/LICENSE-APACHE" "$PACKAGE_DIR/"

mkdir -p "$OUTPUT_DIR"
if [[ "$BARE" == true ]]; then
    BARE_FILE="$OUTPUT_DIR/$NAME"
    if [[ -d "$BARE_FILE" ]]; then
        echo "Error: bare output path is a directory: $BARE_FILE" >&2
        exit 1
    fi
    cp "$PACKAGE_DIR/bin/$BIN" "$BARE_FILE"
fi

ARCHIVE="$OUTPUT_DIR/$NAME.tar.gz"
tar -C "$BUILD_TMP" -czvf "$ARCHIVE" "$NAME"

echo ""
if [[ "$BARE" == true ]]; then
    echo "Created: $BARE_FILE"
fi
echo "Created: $ARCHIVE"
