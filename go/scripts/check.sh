#!/usr/bin/env bash
set -euo pipefail

# Local smoke check for the Go module.
#
# Builds moq-ffi for the host, runs uniffi-bindgen-go, stages everything
# into a tmp dir under the workspace's dist/, and runs `go build`/`go vet`/
# `go test`. Intended for `just go check`.
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
if ! command -v cargo >/dev/null 2>&1; then
    echo "go check: no cargo on PATH, skipping" >&2
    exit 0
fi
if ! command -v uniffi-bindgen-go >/dev/null 2>&1; then
    echo "go check: uniffi-bindgen-go not on PATH, skipping" >&2
    echo "  install: cargo install uniffi-bindgen-go --git https://github.com/NordSecurity/uniffi-bindgen-go --tag v0.7.1+v0.31.0" >&2
    exit 0
fi

HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
echo "go check: building moq-ffi for $HOST_TARGET..."
cargo build --release --package moq-ffi \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml"

TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE_DIR/Cargo.toml" --no-deps |
    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

case "$HOST_TARGET" in
    *-apple-*)
        CDYLIB="$TARGET_BASE/release/libmoq_ffi.dylib"
        STATICLIB="$TARGET_BASE/release/libmoq_ffi.a"
        ;;
    *-windows-*)
        CDYLIB="$TARGET_BASE/release/moq_ffi.dll"
        STATICLIB="$TARGET_BASE/release/moq_ffi.lib"
        ;;
    *)
        CDYLIB="$TARGET_BASE/release/libmoq_ffi.so"
        STATICLIB="$TARGET_BASE/release/libmoq_ffi.a"
        ;;
esac

[[ -f "$CDYLIB" ]] || {
    echo "go check: cdylib not found at $CDYLIB" >&2
    exit 1
}
[[ -f "$STATICLIB" ]] || {
    echo "go check: staticlib not found at $STATICLIB" >&2
    exit 1
}

# Reject unsupported hosts up front; package-ffi.sh derives the cgo
# subdir name from the cargo target via its own mapping.
case "$HOST_TARGET" in
    x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu | x86_64-apple-darwin | aarch64-apple-darwin | x86_64-pc-windows-msvc) ;;
    *)
        echo "go check: unsupported host target $HOST_TARGET" >&2
        exit 1
        ;;
esac

# Stage into the workspace's dist/ (gitignored at repo root).
STAGE_PARENT="$WORKSPACE_DIR/dist"
STAGE_LIBS="$STAGE_PARENT/go-libs/$HOST_TARGET"
STAGE_BINDINGS="$STAGE_PARENT/go-bindings"
STAGE_FFI="$STAGE_PARENT/go-ffi-pkg"
STAGE_WRAPPER="$STAGE_PARENT/go-wrapper-pkg"
rm -rf "$STAGE_LIBS" "$STAGE_BINDINGS" "$STAGE_FFI" "$STAGE_WRAPPER"
mkdir -p "$STAGE_LIBS" "$STAGE_BINDINGS"

cp "$STATICLIB" "$STAGE_LIBS/"

echo "go check: generating bindings..."
uniffi-bindgen-go --library "$CDYLIB" --out-dir "$STAGE_BINDINGS"

# Re-shape bindings dir to match package-ffi.sh's --bindings-dir expectation
# (which wants moq/ directly). Some uniffi-bindgen-go versions nest under
# uniffi/moq/; copy the whole dir so moq.h rides along with moq.go.
if [[ -d "$STAGE_BINDINGS/uniffi/moq" && ! -d "$STAGE_BINDINGS/moq" ]]; then
    cp -R "$STAGE_BINDINGS/uniffi/moq" "$STAGE_BINDINGS/moq"
fi

echo "go check: assembling ffi module..."
bash "$GO_DIR/scripts/package-ffi.sh" \
    --version "0.0.0-dev" \
    --source-dir "$GO_DIR/ffi" \
    --lib-dir "$STAGE_PARENT/go-libs" \
    --bindings-dir "$STAGE_BINDINGS" \
    --output "$STAGE_FFI" \
    --no-archive
FFI_PKG="$STAGE_FFI/moq-ffi-0.0.0-dev-go"

echo "go check: staging wrapper module..."
# Build the wrapper against the freshly-generated ffi via a local replace, so
# the hand-written API is checked against the exact bindings from this tree.
# Nothing is written into go/ffi or go/wrapper; this all lives under dist/.
mkdir -p "$STAGE_WRAPPER/moq"
cp "$GO_DIR/wrapper/go.mod" "$STAGE_WRAPPER/"
cp "$GO_DIR/wrapper/VERSION" "$STAGE_WRAPPER/"
cp "$GO_DIR"/wrapper/moq/*.go "$STAGE_WRAPPER/moq/"
(
    cd "$STAGE_WRAPPER"
    go mod edit -require="github.com/moq-dev/moq-go-ffi@v0.0.0-dev"
    go mod edit -replace="github.com/moq-dev/moq-go-ffi=$FFI_PKG"
)

cd "$STAGE_WRAPPER"
export CGO_ENABLED=1 GOFLAGS=-mod=mod
echo "go check: go vet ./..."
go vet ./...
echo "go check: go build ./..."
go build ./...
echo "go check: go test ./..."
go test ./...
echo "go check: ok"
