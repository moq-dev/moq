#!/usr/bin/env bash
set -euo pipefail

# Assemble the moq-ffi Go module: copy the in-tree source skeleton, drop
# in the uniffi-bindgen-go output, bundle per-target static libs into
# moq/lib/<goos>_<goarch>/, and tar the result.
#
# Designed to run after rs/moq-ffi/build.sh produces per-target outputs
# in $LIB_DIR (one subdir per cargo target) and the uniffi-bindgen-go
# output at $BINDINGS_DIR/moq/moq.go.
#
# Usage:
#   go/scripts/package.sh --version 0.0.0-dev --lib-dir libs --bindings-dir bindings --output dist
#
# Expected $LIB_DIR layout (per cargo target):
#   $LIB_DIR/x86_64-unknown-linux-gnu/libmoq_ffi.a
#   $LIB_DIR/aarch64-unknown-linux-gnu/libmoq_ffi.a
#   $LIB_DIR/x86_64-apple-darwin/libmoq_ffi.a
#   $LIB_DIR/aarch64-apple-darwin/libmoq_ffi.a
#   $LIB_DIR/x86_64-pc-windows-msvc/moq_ffi.lib

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$GO_DIR/.." && pwd)"

VERSION=""
LIB_DIR=""
BINDINGS_DIR=""
OUTPUT_DIR=""
ARCHIVE=true

while [[ $# -gt 0 ]]; do
    case $1 in
        --version)
            VERSION="$2"
            shift 2
            ;;
        --lib-dir)
            LIB_DIR="$2"
            shift 2
            ;;
        --bindings-dir)
            BINDINGS_DIR="$2"
            shift 2
            ;;
        --output)
            OUTPUT_DIR="$2"
            shift 2
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

[[ -z "$VERSION" ]] && {
    echo "Error: --version is required" >&2
    exit 1
}
[[ -z "$LIB_DIR" ]] && {
    echo "Error: --lib-dir is required" >&2
    exit 1
}
[[ -z "$BINDINGS_DIR" ]] && {
    echo "Error: --bindings-dir is required" >&2
    exit 1
}
[[ -z "$OUTPUT_DIR" ]] && OUTPUT_DIR="dist"

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

PKG_NAME="moq-ffi-${VERSION}-go"
PKG_STAGE="$OUTPUT_DIR/$PKG_NAME"
rm -rf "$PKG_STAGE"
mkdir -p "$PKG_STAGE/moq/lib"

# --- 1. Copy in-tree source ---
cp "$GO_DIR/go.mod" "$PKG_STAGE/"
cp "$GO_DIR/README.md" "$PKG_STAGE/"
# Copy hand-written .go files (cgo.go and anything else that lands later).
# Skip the generated moq.go from the source tree (gitignored, shouldn't
# exist there, but defensive).
for f in "$GO_DIR"/moq/*.go; do
    [[ "$(basename "$f")" == "moq.go" ]] && continue
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

# --- 2. Generated uniffi-bindgen-go output ---
# uniffi-bindgen-go emits both moq.go and a C header moq.h. moq.go's cgo
# preamble does `#include <moq.h>`, so the header must ship alongside it or
# consumers hit `fatal error: 'moq.h' file not found` on `go build`.
GENERATED_GO="$BINDINGS_DIR/moq/moq.go"
GENERATED_H="$BINDINGS_DIR/moq/moq.h"
[[ -f "$GENERATED_GO" ]] || {
    echo "Error: uniffi-bindgen-go output not found at $GENERATED_GO" >&2
    exit 1
}
[[ -f "$GENERATED_H" ]] || {
    echo "Error: generated C header not found at $GENERATED_H" >&2
    exit 1
}
cp "$GENERATED_GO" "$PKG_STAGE/moq/moq.go"
cp "$GENERATED_H" "$PKG_STAGE/moq/moq.h"

# --- 3. Per-target static libraries ---
# Entries are "<cargo-target>:<goos>_<goarch>:<libname>"; the GOOS/GOARCH
# subdir name matches the cgo build tags in moq/cgo.go.
GO_LIBS=(
    "x86_64-unknown-linux-gnu:linux_amd64:libmoq_ffi.a"
    "aarch64-unknown-linux-gnu:linux_arm64:libmoq_ffi.a"
    "x86_64-apple-darwin:darwin_amd64:libmoq_ffi.a"
    "aarch64-apple-darwin:darwin_arm64:libmoq_ffi.a"
    "x86_64-pc-windows-msvc:windows_amd64:moq_ffi.lib"
)
STAGED_ANY=false
for entry in "${GO_LIBS[@]}"; do
    target="${entry%%:*}"
    rest="${entry#*:}"
    goarch="${rest%%:*}"
    libname="${rest##*:}"
    src="$LIB_DIR/$target/$libname"
    if [[ -f "$src" ]]; then
        dest="$PKG_STAGE/moq/lib/$goarch"
        mkdir -p "$dest"
        cp "$src" "$dest/"
        echo "  go lib $goarch <- $target"
        STAGED_ANY=true
    else
        echo "  go lib $goarch: skipped, $src missing"
    fi
done

if [[ "$STAGED_ANY" != true ]]; then
    echo "Error: no per-target libs were staged; check --lib-dir layout" >&2
    exit 1
fi

# --- 4. Minimal consumer-facing README rewrite ---
# The full developer README lives in the monorepo; the staged copy
# (which ends up on moq-dev/moq-go) gets a thin orientation pointer.
cat >"$PKG_STAGE/README.md" <<EOF
# Moq (Go module)

Auto-generated mirror of the Go module for [Media over QUIC](https://github.com/moq-dev/moq).

Source, issues, and pull requests live in [moq-dev/moq](https://github.com/moq-dev/moq); this repo only carries tagged Go module releases.

## Install

\`\`\`bash
go get github.com/moq-dev/moq-go@v${VERSION}
\`\`\`

The module bundles prebuilt native libraries for \`linux/amd64\`, \`linux/arm64\`, \`darwin/amd64\`, \`darwin/arm64\` (\`libmoq_ffi.a\`), and \`windows/amd64\` (\`moq_ffi.lib\`); cgo selects the right one automatically.

See [moq-dev/moq/go/README.md](https://github.com/moq-dev/moq/blob/main/go/README.md) for usage and the release process.

Licensed under MIT OR Apache-2.0.
EOF

echo ""
echo "Staged: $PKG_STAGE"

if [[ "$ARCHIVE" == true ]]; then
    ARCHIVE_PATH="$OUTPUT_DIR/${PKG_NAME}.tar.gz"
    tar -czf "$ARCHIVE_PATH" -C "$OUTPUT_DIR" "$PKG_NAME"
    echo "Created: $ARCHIVE_PATH"
fi
