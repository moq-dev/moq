#!/usr/bin/env bash
set -euo pipefail

# Stage the Go modules from THIS checkout: build moq-ffi for the host, run
# uniffi-bindgen-go, and assemble the ffi + wrapper modules with the wrapper
# wired to the local ffi by a `replace`.
#
# Progress goes to stderr. stdout is exactly two lines, the staged ffi module
# directory then the staged wrapper module directory, so a caller can `replace`
# its own module against them and build Go code against these bindings.
#
# Usage:
#   go/scripts/stage.sh [--output DIR]
#
# Optional:
#   --output DIR  stage parent (default: <workspace>/dist, gitignored)
#
# The generated bindings land in <DIR>/go-bindings. Set MOQ_FFI_PROFILE=release
# for an optimized cdylib.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$GO_DIR/.." && pwd)"

STAGE_PARENT="$WORKSPACE_DIR/dist"

while [[ $# -gt 0 ]]; do
    case $1 in
        --output)
            STAGE_PARENT="$2"
            shift 2
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

# Absolute, because the paths below are printed for a caller in another
# directory and fed to `go mod edit -replace`, which resolves a relative target
# against the module being edited rather than the cwd.
mkdir -p "$STAGE_PARENT"
STAGE_PARENT=$(cd "$STAGE_PARENT" && pwd)

command -v go >/dev/null 2>&1 || {
    echo "go stage: no go on PATH" >&2
    exit 1
}
command -v cargo >/dev/null 2>&1 || {
    echo "go stage: no cargo on PATH" >&2
    exit 1
}
command -v uniffi-bindgen-go >/dev/null 2>&1 || {
    echo "go stage: uniffi-bindgen-go not on PATH" >&2
    echo "  install: cargo install uniffi-bindgen-go --git https://github.com/kixelated/uniffi-bindgen-go --rev 4f79e52bd8f518e5fa4d7acff9e586aee21e12a0 --locked" >&2
    exit 1
}

HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
# Debug by default. This is a compile-and-test gate, not a benchmark, and a
# release build of moq-ffi shares no artifacts with the debug ones `just check`
# and `just test` already produce, so it was a third full compile of the
# dependency tree (~5 min of CI on its own, plus a whole target/release tree on
# a runner that was already tight on disk). Set MOQ_FFI_PROFILE=release for an
# optimized cdylib; the shipped artifacts are built by rs/moq-ffi/build.sh,
# which is release regardless.
PROFILE="${MOQ_FFI_PROFILE:-debug}"
# Expanded as ${CARGO_PROFILE[@]+...} at the use sites: macOS ships bash 3.2,
# where expanding an empty array under `set -u` is an "unbound variable" error.
CARGO_PROFILE=()
[[ "$PROFILE" == "release" ]] && CARGO_PROFILE=(--release)

echo "go stage: building moq-ffi for $HOST_TARGET..." >&2
cargo build --locked ${CARGO_PROFILE[@]+"${CARGO_PROFILE[@]}"} --package moq-ffi \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml" >&2

TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE_DIR/Cargo.toml" --no-deps |
    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

case "$HOST_TARGET" in
    *-apple-*)
        CDYLIB="$TARGET_BASE/$PROFILE/libmoq_ffi.dylib"
        STATICLIB="$TARGET_BASE/$PROFILE/libmoq_ffi.a"
        ;;
    *-windows-*)
        CDYLIB="$TARGET_BASE/$PROFILE/moq_ffi.dll"
        STATICLIB="$TARGET_BASE/$PROFILE/moq_ffi.lib"
        ;;
    *)
        CDYLIB="$TARGET_BASE/$PROFILE/libmoq_ffi.so"
        STATICLIB="$TARGET_BASE/$PROFILE/libmoq_ffi.a"
        ;;
esac

[[ -f "$CDYLIB" ]] || {
    echo "go stage: cdylib not found at $CDYLIB" >&2
    exit 1
}
[[ -f "$STATICLIB" ]] || {
    echo "go stage: staticlib not found at $STATICLIB" >&2
    exit 1
}

# Reject unsupported hosts up front; package-ffi.sh derives the cgo
# subdir name from the cargo target via its own mapping.
case "$HOST_TARGET" in
    x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu | aarch64-apple-darwin | x86_64-pc-windows-msvc) ;;
    *)
        echo "go stage: unsupported host target $HOST_TARGET" >&2
        exit 1
        ;;
esac

STAGE_LIBS="$STAGE_PARENT/go-libs/$HOST_TARGET"
STAGE_BINDINGS="$STAGE_PARENT/go-bindings"
STAGE_FFI="$STAGE_PARENT/go-ffi-pkg"
STAGE_WRAPPER="$STAGE_PARENT/go-wrapper-pkg"
rm -rf "$STAGE_LIBS" "$STAGE_BINDINGS" "$STAGE_FFI" "$STAGE_WRAPPER"
mkdir -p "$STAGE_LIBS" "$STAGE_BINDINGS"

cp "$STATICLIB" "$STAGE_LIBS/"

echo "go stage: generating bindings..." >&2
uniffi-bindgen-go --library "$CDYLIB" --out-dir "$STAGE_BINDINGS" >&2

# Re-shape bindings dir to match package-ffi.sh's --bindings-dir expectation
# (which wants moq/ directly). Some uniffi-bindgen-go versions nest under
# uniffi/moq/; copy the whole dir so moq.h rides along with moq.go.
if [[ -d "$STAGE_BINDINGS/uniffi/moq" && ! -d "$STAGE_BINDINGS/moq" ]]; then
    cp -R "$STAGE_BINDINGS/uniffi/moq" "$STAGE_BINDINGS/moq"
fi

echo "go stage: assembling ffi module..." >&2
# --skip-size-check because the lib above is a plain host build, unrelated to
# what the mirror publishes. It is a debug build by default now, so it carries
# full line-table debug info and measured 619 MiB against a 100 MiB limit; even
# at MOQ_FFI_PROFILE=release it skips the thin LTO that rs/moq-ffi/build.sh
# applies on the publish path. Enforcing the limit here fails on a lib nobody
# publishes.
bash "$SCRIPT_DIR/package-ffi.sh" \
    --version "0.0.0-dev" \
    --source-dir "$GO_DIR/ffi" \
    --lib-dir "$STAGE_PARENT/go-libs" \
    --bindings-dir "$STAGE_BINDINGS" \
    --output "$STAGE_FFI" \
    --no-archive \
    --skip-size-check >&2
FFI_PKG="$STAGE_FFI/moq-ffi-0.0.0-dev-go"

echo "go stage: staging wrapper module..." >&2
# Run the real publish packager (so callers exercise the same assembly), then
# point it at the freshly-generated ffi via a local replace so the hand-written
# API builds against the exact bindings from this tree. Nothing is written into
# go/ffi or go/wrapper; this all lives under the stage dir.
WRAPPER_LINE=$(tr -d '[:space:]' <"$GO_DIR/wrapper/VERSION")
bash "$SCRIPT_DIR/package-wrapper.sh" \
    --line "$WRAPPER_LINE" \
    --ffi-version "0.0.0-dev" \
    --source-dir "$GO_DIR/wrapper" \
    --output "$STAGE_WRAPPER" \
    --skip-tidy \
    --no-archive >&2
WRAPPER_PKG="$STAGE_WRAPPER/moq-go-${WRAPPER_LINE}-wrapper"
(
    cd "$WRAPPER_PKG"
    go mod edit -replace="moq.dev/moq-ffi=$FFI_PKG"
)

printf '%s\n%s\n' "$FFI_PKG" "$WRAPPER_PKG"
