#!/usr/bin/env bash
set -euo pipefail

# Build and package a workspace binary for release.
# Usage: ./package-binary.sh --crate CRATE [--target TARGET] [--version VERSION] [--output DIR]
#
# Builds via `nix build .#<crate>` against the flake-pinned toolchain so
# artifacts are reproducible across hosts. The resulting tarball matches
# the layout consumed by the Homebrew tap templates in
# .github/homebrew/Formula/.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$RS_DIR/.." && pwd)"

CRATE=""
TARGET=""
VERSION=""
OUTPUT_DIR="dist"

while [[ $# -gt 0 ]]; do
    case $1 in
        --crate | --target | --version | --output)
            if [[ $# -lt 2 ]]; then
                echo "Error: $1 requires a value" >&2
                exit 1
            fi
            case $1 in
                --crate) CRATE="$2" ;;
                --target) TARGET="$2" ;;
                --version) VERSION="$2" ;;
                --output) OUTPUT_DIR="$2" ;;
            esac
            shift 2
            ;;
        -h | --help)
            echo "Usage: $0 --crate CRATE [--target TARGET] [--version VERSION] [--output DIR]"
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

# This script builds the native nix output for the host. Cross-compilation
# isn't wired up, so an explicit --target that disagrees with the host
# would silently mislabel the archive. CI keeps target == host by matching
# each matrix entry to a runner of the right arch.
HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
if [[ "$TARGET" != "$HOST_TARGET" ]]; then
    echo "Error: --target ($TARGET) does not match host target ($HOST_TARGET)." >&2
    echo "This script builds native nix outputs; refusing to mislabel the archive." >&2
    exit 1
fi

echo "Building $CRATE for $TARGET via nix..."

BUILD_TMP="$(mktemp -d)"
trap 'rm -rf "$BUILD_TMP"' EXIT
RESULT_LINK="$BUILD_TMP/result"
nix build "$WORKSPACE_DIR#$CRATE" --out-link "$RESULT_LINK"

# Locate the built binary. Crane installs to result/bin/<binary>.
# By convention each of our binaries shares its crate name.
BIN_FILE="$RESULT_LINK/bin/$CRATE"
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
cp -L "$BIN_FILE" "$PACKAGE_DIR/bin/$CRATE"
chmod 0755 "$PACKAGE_DIR/bin/$CRATE"

# On macOS the nix toolchain links the nix-store copy of libiconv, whose
# absolute path doesn't exist on a user's Mac, so dyld aborts at startup
# (the brew channel's "Library not loaded: /nix/store/.../libiconv.2.dylib").
# Rewrite the leaked paths to load from the system dyld shared cache (via
# /usr/lib), like a plain `cargo build` does. scrub-macho.sh also asserts no
# /nix path survives, so this can't silently ship again.
if [[ "$(uname)" == "Darwin" ]]; then
    bin="$PACKAGE_DIR/bin/$CRATE"
    "$SCRIPT_DIR/scrub-macho.sh" "$bin"

    # Prove dyld actually loads the scrubbed binary. host==target is enforced
    # above, so it runs natively; --help triggers dyld's dependency load (the
    # exact step that aborted on a clean Mac) before clap exits 0.
    if ! "$bin" --help >/dev/null 2>&1; then
        echo "Error: scrubbed $bin failed to launch (dyld load failure?)." >&2
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
