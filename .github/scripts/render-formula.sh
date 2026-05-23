#!/usr/bin/env bash
set -euo pipefail

# Render a Homebrew formula template by substituting __VERSION__ and the
# four __SHA256_<TARGET>__ placeholders.
#
# Usage:
#   render-formula.sh \
#     --template <path/to/crate.rb.tmpl> \
#     --version <semver> \
#     --release-dir <dir-of-downloaded-tarballs> \
#     --crate <crate-name> \
#     --output <path/to/crate.rb>
#
# The release dir must contain the four tarballs named like
# <crate>-<version>-<target>.tar.gz for each of:
#   aarch64-apple-darwin
#   x86_64-apple-darwin
#   aarch64-unknown-linux-gnu
#   x86_64-unknown-linux-gnu

TEMPLATE=""
VERSION=""
RELEASE_DIR=""
CRATE=""
OUTPUT=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --template|--version|--release-dir|--crate|--output)
            if [[ $# -lt 2 ]]; then
                echo "Error: $1 requires a value" >&2
                exit 1
            fi
            case $1 in
                --template)    TEMPLATE="$2" ;;
                --version)     VERSION="$2" ;;
                --release-dir) RELEASE_DIR="$2" ;;
                --crate)       CRATE="$2" ;;
                --output)      OUTPUT="$2" ;;
            esac
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 --template T --version V --release-dir D --crate C --output O"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

for var in TEMPLATE VERSION RELEASE_DIR CRATE OUTPUT; do
    if [[ -z "${!var}" ]]; then
        echo "Error: --${var,,} is required" >&2
        exit 1
    fi
done

if [[ ! -f "$TEMPLATE" ]]; then
    echo "Error: template not found: $TEMPLATE" >&2
    exit 1
fi

targets=(
    "aarch64-apple-darwin AARCH64_APPLE_DARWIN"
    "x86_64-apple-darwin X86_64_APPLE_DARWIN"
    "aarch64-unknown-linux-gnu AARCH64_UNKNOWN_LINUX_GNU"
    "x86_64-unknown-linux-gnu X86_64_UNKNOWN_LINUX_GNU"
)

# Read template once and apply all substitutions in-memory.
rendered=$(cat "$TEMPLATE")
rendered="${rendered//__VERSION__/$VERSION}"

for entry in "${targets[@]}"; do
    target="${entry%% *}"
    placeholder="${entry##* }"
    tarball="$RELEASE_DIR/$CRATE-$VERSION-$target.tar.gz"

    if [[ ! -f "$tarball" ]]; then
        echo "Error: tarball not found: $tarball" >&2
        exit 1
    fi

    sha=$(sha256sum "$tarball" | awk '{print $1}')
    echo "  $target: $sha"
    rendered="${rendered//__SHA256_${placeholder}__/$sha}"
done

# Sanity check: no placeholders left.
if grep -q '__[A-Z0-9_]\+__' <<<"$rendered"; then
    echo "Error: unsubstituted placeholders remain:" >&2
    grep -o '__[A-Z0-9_]\+__' <<<"$rendered" | sort -u >&2
    exit 1
fi

mkdir -p "$(dirname "$OUTPUT")"
printf '%s\n' "$rendered" > "$OUTPUT"
echo "Wrote: $OUTPUT"
