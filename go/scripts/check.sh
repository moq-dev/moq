#!/usr/bin/env bash
set -euo pipefail

# Local smoke check for the Go module.
#
# Stages the ffi + wrapper modules from this checkout (scripts/stage.sh) and
# runs `go build`/`go vet`/`go test` against them. Intended for `just go check`.
#
# The main repo stays binary-free: no `.a` or generated `.go` files land
# in go/ during local development. Everything happens in dist/, which is
# already gitignored at the repo root.
#
# Skipped cleanly on hosts without `go`, `cargo`, or `uniffi-bindgen-go`.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$GO_DIR/.." && pwd)"

if ! command -v go >/dev/null 2>&1; then
    echo "go check: no go on PATH, skipping" >&2
    exit 0
fi
# The publish state machine needs neither cargo nor generated bindings, so it
# runs ahead of the gates that skip a partial toolchain.
bash "$SCRIPT_DIR/publish-wrapper.test.sh"

if ! command -v cargo >/dev/null 2>&1; then
    echo "go check: no cargo on PATH, skipping" >&2
    exit 0
fi
if ! command -v uniffi-bindgen-go >/dev/null 2>&1; then
    echo "go check: uniffi-bindgen-go not on PATH, skipping" >&2
    echo "  install: cargo install uniffi-bindgen-go --git https://github.com/kixelated/uniffi-bindgen-go --rev 4f79e52bd8f518e5fa4d7acff9e586aee21e12a0 --locked" >&2
    exit 0
fi

# Stage into the workspace's dist/ (gitignored at repo root).
STAGE_PARENT="$WORKSPACE_DIR/dist"
STAGED=$(bash "$SCRIPT_DIR/stage.sh" --output "$STAGE_PARENT")
WRAPPER_PKG=$(printf '%s\n' "$STAGED" | sed -n 2p)

echo "go check: checking error sentinels..."
bash "$SCRIPT_DIR/check-errors.sh" \
    "$STAGE_PARENT/go-bindings/moq/moq.go" \
    "$GO_DIR/wrapper/moq/errors.go" \
    "$GO_DIR/wrapper/moq/errors_test.go"

cd "$WRAPPER_PKG"
export CGO_ENABLED=1 GOFLAGS=-mod=mod
echo "go check: go vet ./..."
go vet ./...
echo "go check: go build ./..."
go build ./...
echo "go check: go test -race ./..."
go test -race ./...
echo "go check: ok"
