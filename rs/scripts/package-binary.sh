#!/usr/bin/env bash
set -euo pipefail

# Build and package a workspace binary for release.
# Usage: ./package-binary.sh --crate CRATE [--bin NAME] [--target TARGET] [--version VERSION] [--output DIR]
#
# --bin overrides the binary/command name when it differs from the crate (e.g.
# the `moq-cli` crate ships its binary as `moq`); it defaults to the crate name.
#
# Builds via `nix build .#<crate>` against the flake-pinned toolchain so
# artifacts are reproducible across hosts. The resulting tarball matches
# the layout consumed by the Homebrew tap templates in
# .github/homebrew/Formula/.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$RS_DIR/.." && pwd)"

CRATE=""
BIN=""
TARGET=""
VERSION=""
OUTPUT_DIR="dist"

while [[ $# -gt 0 ]]; do
    case $1 in
        --crate | --bin | --target | --version | --output)
            if [[ $# -lt 2 ]]; then
                echo "Error: $1 requires a value" >&2
                exit 1
            fi
            case $1 in
                --crate) CRATE="$2" ;;
                --bin) BIN="$2" ;;
                --target) TARGET="$2" ;;
                --version) VERSION="$2" ;;
                --output) OUTPUT_DIR="$2" ;;
            esac
            shift 2
            ;;
        -h | --help)
            echo "Usage: $0 --crate CRATE [--bin NAME] [--target TARGET] [--version VERSION] [--output DIR]"
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

# Native builds use the bare flake output. The one supported cross is the
# Intel mac release built on an Apple Silicon runner (the Determinate Nix
# installer dropped Intel macOS): the flake exposes a per-target output for
# it (nix/overlay.nix) and Apple's clang cross-compiles natively, so the only
# emulation is the Rosetta smoke test below. Any other host/target mismatch
# would silently mislabel the archive, so it's rejected.
HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
NIX_ATTR="$CRATE"
if [[ "$TARGET" != "$HOST_TARGET" ]]; then
    if [[ "$HOST_TARGET" == "aarch64-apple-darwin" && "$TARGET" == "x86_64-apple-darwin" ]]; then
        NIX_ATTR="$CRATE-$TARGET"
    else
        echo "Error: unsupported cross ($HOST_TARGET -> $TARGET)." >&2
        echo "Only aarch64-apple-darwin -> x86_64-apple-darwin is wired up; refusing to mislabel the archive." >&2
        exit 1
    fi
fi

echo "Building $CRATE for $TARGET via nix (output: $NIX_ATTR)..."

BUILD_TMP="$(mktemp -d)"
trap 'rm -rf "$BUILD_TMP"' EXIT
RESULT_LINK="$BUILD_TMP/result"
nix build "$WORKSPACE_DIR#$NIX_ATTR" --out-link "$RESULT_LINK"

# Locate the built binary. Crane installs to result/bin/<binary>. The binary
# name is usually the crate name; the `moq-cli` crate ships as `moq`.
BIN_FILE="$RESULT_LINK/bin/$BIN"
if [[ ! -f "$BIN_FILE" ]]; then
    echo "Error: no binary found at $BIN_FILE" >&2
    echo "Contents of $RESULT_LINK/bin:" >&2
    ls "$RESULT_LINK/bin" >&2 || true
    exit 1
fi

NAME="$CRATE-$VERSION-$TARGET"
PACKAGE_DIR="$OUTPUT_DIR/$NAME"

echo "Packaging $NAME..."
rm -rf "$PACKAGE_DIR"
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

cd "$OUTPUT_DIR"
ARCHIVE="$NAME.tar.gz"
tar -czvf "$ARCHIVE" "$NAME"
rm -rf "$NAME"

echo ""
echo "Created: $OUTPUT_DIR/$ARCHIVE"
echo "$ARCHIVE"
