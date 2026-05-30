#!/usr/bin/env bash
set -euo pipefail

# Assemble the moq-go ergonomic wrapper module: copy the in-tree wrapper
# skeleton, rewrite its github.com/moq-dev/moq-go-ffi `require` to the target
# ffi version, generate go.sum, and tar the result.
#
# The staged tree is deliberately PATCH-INDEPENDENT: it carries the MAJOR.MINOR
# line (VERSION), the ffi require, and the hand-written source, but nothing that
# encodes the wrapper's own patch number. publish-wrapper.sh decides the patch
# from the mirror's existing tags only after confirming the tree actually
# changed, so a no-op trigger (e.g. an ffi tag that didn't move the ffi version)
# produces an identical tree and no release.
#
# Usage:
#   go/scripts/package-wrapper.sh --line 0.3 --ffi-version 0.2.18 --output dist
#
# Optional:
#   --source-dir DIR  in-tree wrapper skeleton (default: go/wrapper)
#   --skip-tidy       skip `go mod tidy` (no go.sum); use for dry-runs where the
#                     target ffi tag may not be published yet
#   --no-archive

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$GO_ROOT/.." && pwd)"

LINE=""
FFI_VERSION=""
OUTPUT_DIR=""
SOURCE_DIR=""
ARCHIVE=true
TIDY=true

while [[ $# -gt 0 ]]; do
    case $1 in
        --line)
            LINE="$2"
            shift 2
            ;;
        --ffi-version)
            FFI_VERSION="$2"
            shift 2
            ;;
        --output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --source-dir)
            SOURCE_DIR="$2"
            shift 2
            ;;
        --skip-tidy)
            TIDY=false
            shift
            ;;
        --no-archive)
            ARCHIVE=false
            shift
            ;;
        -h | --help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

[[ -z "$SOURCE_DIR" ]] && SOURCE_DIR="$GO_ROOT/wrapper"
SOURCE_DIR="$(cd "$SOURCE_DIR" && pwd)"

[[ -z "$LINE" ]] && {
    echo "Error: --line (MAJOR.MINOR) is required" >&2
    exit 1
}
[[ -z "$FFI_VERSION" ]] && {
    echo "Error: --ffi-version is required" >&2
    exit 1
}
[[ -z "$OUTPUT_DIR" ]] && OUTPUT_DIR="dist"

# Accept a leading v on the ffi version, normalize it off.
FFI_VERSION="${FFI_VERSION#v}"

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

PKG_NAME="moq-go-${LINE}-wrapper"
PKG_STAGE="$OUTPUT_DIR/$PKG_NAME"
rm -rf "$PKG_STAGE"
mkdir -p "$PKG_STAGE/moq"

# --- 1. Copy in-tree source ---
cp "$SOURCE_DIR/go.mod" "$PKG_STAGE/"
cp "$SOURCE_DIR/VERSION" "$PKG_STAGE/"
for f in "$SOURCE_DIR"/moq/*.go; do
    cp "$f" "$PKG_STAGE/moq/"
done

# Dual-license files lifted from the workspace root.
for license in LICENSE-MIT LICENSE-APACHE; do
    [[ -f "$WORKSPACE_DIR/$license" ]] || {
        echo "Error: missing $WORKSPACE_DIR/$license" >&2
        exit 1
    }
    cp "$WORKSPACE_DIR/$license" "$PKG_STAGE/$license"
done

# --- 2. Pin the ffi require (replaces the in-tree v0.0.0 placeholder) ---
# MVS resolves to the maximum version across the build graph, so this require is
# a floor: pinning it to the latest published ffi makes `moq-go@latest` pull an
# ffi at least this new. Drop any replace defensively (the committed go.mod has
# none, but `just go check` adds one locally).
(
    cd "$PKG_STAGE"
    go mod edit -require="github.com/moq-dev/moq-go-ffi@v${FFI_VERSION}"
    go mod edit -dropreplace="github.com/moq-dev/moq-go-ffi"
)

# --- 3. Thin consumer-facing README (full dev README lives in the monorepo) ---
cat >"$PKG_STAGE/README.md" <<EOF
# moq-go (Go module)

Auto-generated mirror of the ergonomic Go wrapper for [Media over QUIC](https://github.com/moq-dev/moq).

Source, issues, and pull requests live in [moq-dev/moq](https://github.com/moq-dev/moq); this repo only carries tagged Go module releases.

## Install

\`\`\`bash
go get github.com/moq-dev/moq-go@latest
\`\`\`

\`\`\`go
import "github.com/moq-dev/moq-go/moq"
\`\`\`

Pure Go on top of the raw [github.com/moq-dev/moq-go-ffi](https://github.com/moq-dev/moq-go-ffi) bindings, which carry the prebuilt native libraries. \`CGO_ENABLED=1\` is required (the default on Unix).

See [moq-dev/moq/go/wrapper/README.md](https://github.com/moq-dev/moq/blob/main/go/wrapper/README.md) for usage and the release process.

Licensed under MIT OR Apache-2.0.
EOF

# --- 4. Resolve go.sum (skipped for dry-runs against an unpublished ffi tag) ---
# GOPROXY=direct fetches the freshly-pushed ffi tag straight from its mirror,
# dodging proxy.golang.org caching lag right after a release.
if [[ "$TIDY" == true ]]; then
    (
        cd "$PKG_STAGE"
        GOFLAGS=-mod=mod GOPROXY="${GOPROXY:-direct}" go mod tidy
    )
else
    echo "  skipping go mod tidy (no go.sum staged)"
fi

echo ""
echo "Staged: $PKG_STAGE"

if [[ "$ARCHIVE" == true ]]; then
    ARCHIVE_PATH="$OUTPUT_DIR/${PKG_NAME}.tar.gz"
    tar -czf "$ARCHIVE_PATH" -C "$OUTPUT_DIR" "$PKG_NAME"
    echo "Created: $ARCHIVE_PATH"
fi
