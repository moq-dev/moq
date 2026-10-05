#!/usr/bin/env bash
set -euo pipefail

# Local smoke check for the Go module.
#
# Stages the ffi + wrapper modules from this checkout (sh/go/stage.sh) and
# runs `go build`/`go vet`/`go test` against them. Intended for `just go check`.
#
# The main repo stays binary-free: no `.a` or generated `.go` files land
# in go/ during local development. Everything happens in dist/, which is
# already gitignored at the repo root.
#
# Skipped cleanly on hosts without `go`, `cargo`, or `uniffi-bindgen-go`.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GO_DIR="$(cd "$SCRIPT_DIR/../../go" && pwd)"
WORKSPACE_DIR="$(cd "$GO_DIR/.." && pwd)"

if ! command -v go >/dev/null 2>&1; then
    echo "go check: no go on PATH, skipping" >&2
    exit 0
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "go check: no cargo on PATH, skipping" >&2
    exit 0
fi
if ! command -v uniffi-bindgen-go >/dev/null 2>&1; then
    echo "go check: uniffi-bindgen-go not on PATH, skipping" >&2
    echo "  install: cargo install uniffi-bindgen-go --git https://github.com/kixelated/uniffi-bindgen-go --rev v0.10.0-kixelated.1+v0.32.0 --locked" >&2
    exit 0
fi

# Stage into the workspace's dist/ (gitignored at repo root).
STAGE_PARENT="$WORKSPACE_DIR/dist"
STAGED=$(bash "$SCRIPT_DIR/stage.sh" --output "$STAGE_PARENT")
WRAPPER_PKG=$(printf '%s\n' "$STAGED" | sed -n 2p)

echo "go check: checking error sentinels..."
bash "$SCRIPT_DIR/check-errors.sh" \
    "$STAGE_PARENT/go-bindings/moq/moq.go" \
    "$GO_DIR/wrapper/errors.go" \
    "$GO_DIR/wrapper/errors_test.go"

# Keep extracted samples in the staged module so they compile against this
# checkout without becoming part of the published wrapper.
mkdir -p "$WRAPPER_PKG/docs"
cp "$SCRIPT_DIR/doc_prelude.go" "$WRAPPER_PKG/docs/prelude.go"
bash "$WORKSPACE_DIR/doc/lib/samples.sh" go \
    "$WORKSPACE_DIR/doc/lib/go/"*.md "$GO_DIR/wrapper/README.md" \
    >"$WRAPPER_PKG/docs/samples.go"

cd "$WRAPPER_PKG"
export CGO_ENABLED=1 GOFLAGS=-mod=mod
echo "go check: go vet ./..."
go vet ./...
echo "go check: go build ./..."
go build ./...
echo "go check: go test -race ./..."
go test -race ./...
echo "go check: ok"
