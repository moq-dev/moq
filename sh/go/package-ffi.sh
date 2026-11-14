#!/usr/bin/env bash
set -euo pipefail

# Assemble the moq-go-ffi Go module: copy the in-tree source skeleton, drop
# in the uniffi-bindgen-go output, bundle per-target static libs into
# moq/lib/<goos>_<goarch>/, and tar the result.
#
# Designed to run after rs/moq-ffi/build.sh produces per-target outputs
# in $LIB_DIR (one subdir per cargo target) and the uniffi-bindgen-go
# output at $BINDINGS_DIR/moq/moq.go.
#
# Usage:
#   sh/go/package-ffi.sh --version 0.0.0-dev --lib-dir libs --bindings-dir bindings --output dist
#
# Optional:
#   --source-dir DIR   in-tree ffi module skeleton (default: go/ffi)
#   --skip-size-check  don't enforce GitHub's 100 MiB per-file push limit, for
#                      callers staging libs that are never pushed to the mirror
#
# Expected $LIB_DIR layout (per cargo target):
#   $LIB_DIR/x86_64-unknown-linux-gnu/libmoq_ffi.a
#   $LIB_DIR/aarch64-unknown-linux-gnu/libmoq_ffi.a
#   $LIB_DIR/aarch64-apple-darwin/libmoq_ffi.a
#   $LIB_DIR/x86_64-pc-windows-msvc/moq_ffi.lib

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GO_ROOT="$(cd "$SCRIPT_DIR/../../go" && pwd)"
WORKSPACE_DIR="$(cd "$GO_ROOT/.." && pwd)"

VERSION=""
LIB_DIR=""
BINDINGS_DIR=""
OUTPUT_DIR=""
SOURCE_DIR=""
ARCHIVE=true
SIZE_CHECK=true

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
        --source-dir)
            SOURCE_DIR="$2"
            shift 2
            ;;
        --no-archive)
            ARCHIVE=false
            shift
            ;;
        --skip-size-check)
            SIZE_CHECK=false
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

[[ -z "$SOURCE_DIR" ]] && SOURCE_DIR="$GO_ROOT/ffi"
SOURCE_DIR="$(cd "$SOURCE_DIR" && pwd)"

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
cp "$SOURCE_DIR/go.mod" "$PKG_STAGE/"
cp "$SOURCE_DIR/README.md" "$PKG_STAGE/"
# Copy hand-written .go files (cgo.go and anything else that lands later).
# Skip the generated moq.go from the source tree (gitignored, shouldn't
# exist there, but defensive).
for f in "$SOURCE_DIR"/moq/*.go; do
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
# Go 1.26.11 and 1.27.4 refuse to link a cgo archive with LLVM bitcode or GCC LTO
# sections ("disallowed LTO section"). Rust's prebuilt compiler_builtins embeds
# bitcode and skips LTO, so every staticlib carries it; the linker never reads
# it. llvm-objcopy can't rewrite the MSVC .lib in one pass (its raw-dylib
# import members are unsupported), so strip the offending members one by one
# and replace them in place. rustc's own llvm-tools read every target's format.
LLVM_BIN="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | awk '/^host:/ {print $2}')/bin"
for tool in llvm-ar llvm-objcopy llvm-objdump; do
    [[ -x "$LLVM_BIN/$tool" ]] || {
        echo "Error: $LLVM_BIN/$tool missing; run: rustup component add llvm-tools" >&2
        exit 1
    }
done

# Print each archive member holding a section Go rejects (see isDisallowedLTOSection
# in cmd/go/internal/work/security.go; Mach-O's __LLVM sections show unqualified).
lto_members() {
    "$LLVM_BIN/llvm-objdump" -h "$1" | awk '
        /:\tfile format / { m = $0; sub(/^.*\(/, "", m); sub(/\):\tfile format .*$/, "", m) }
        $2 ~ /^(\.llvmbc|\.llvmcmd|__bitcode|__cmdline|\.llvm\.lto.*|\.gnu\.lto_.*|\.gnu\.debuglto_.*)$/ { print m }
    ' | sort -u
}

strip_lto() {
    local lib="$1" work m dups members=()
    # A read loop, not mapfile: macOS ships bash 3.2.
    while IFS= read -r m; do members+=("$m"); done < <(lto_members "$lib")
    [[ "${#members[@]}" -eq 0 ]] && return
    # `llvm-ar x`/`r` address members by name, so a duplicate name is ambiguous.
    dups=$("$LLVM_BIN/llvm-ar" t "$lib" | sort | uniq -d | grep -Fxf <(printf '%s\n' "${members[@]}") || true)
    if [[ -n "$dups" ]]; then
        echo "Error: $lib has duplicate members carrying LTO sections:" >&2
        printf '  %s\n' "$dups" >&2
        exit 1
    fi
    work=$(mktemp -d "$OUTPUT_DIR/strip.XXXXXX")
    (
        cd "$work"
        "$LLVM_BIN/llvm-ar" x "$lib" "${members[@]}"
        for m in "${members[@]}"; do
            "$LLVM_BIN/llvm-objcopy" --remove-section=.llvmbc --remove-section=.llvmcmd \
                --remove-section=__LLVM,__bitcode --remove-section=__LLVM,__cmdline "$m"
        done
        "$LLVM_BIN/llvm-ar" r "$lib" "${members[@]}"
    )
    rm -rf "$work"
    m=$(lto_members "$lib")
    if [[ -n "$m" ]]; then
        echo "Error: $lib still has LTO sections Go rejects in:" >&2
        printf '%s\n' "$m" | sed 's/^/  /' >&2
        exit 1
    fi
}

# Entries are "<cargo-target>:<goos>_<goarch>:<libname>"; the GOOS/GOARCH
# subdir name matches the cgo build tags in moq/cgo.go.
GO_LIBS=(
    "x86_64-unknown-linux-gnu:linux_amd64:libmoq_ffi.a"
    "aarch64-unknown-linux-gnu:linux_arm64:libmoq_ffi.a"
    "aarch64-apple-darwin:darwin_arm64:libmoq_ffi.a"
    "x86_64-pc-windows-msvc:windows_amd64:moq_ffi.lib"
)
# GitHub's pre-receive hook rejects any file >= 100 MiB. The mirror commits
# these libs into git, so an oversized lib fails the publish push (GH001).
# Catch it here, where the error names the file and points at the fix, rather
# than after a multi-target build burns an hour to die at `git push`.
#
# Only meaningful for artifacts actually destined for the mirror. `go check`
# reuses this script to stage a throwaway module against a debug staticlib,
# which carries enough debug info to blow the limit and is never pushed, so it
# passes --skip-size-check.
GITHUB_FILE_LIMIT=$((100 * 1024 * 1024))
STAGED_ANY=false
OVERSIZED=()
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
        chmod u+w "$dest/$libname"
        strip_lto "$dest/$libname"
        size=$(wc -c <"$dest/$libname")
        echo "  go lib $goarch <- $target ($((size / 1024 / 1024)) MiB)"
        [[ "$size" -ge "$GITHUB_FILE_LIMIT" ]] && OVERSIZED+=("$goarch/$libname: $((size / 1024 / 1024)) MiB")
        STAGED_ANY=true
    else
        echo "  go lib $goarch: skipped, $src missing"
    fi
done

if [[ "$STAGED_ANY" != true ]]; then
    echo "Error: no per-target libs were staged; check --lib-dir layout" >&2
    exit 1
fi

if [[ "$SIZE_CHECK" == true && "${#OVERSIZED[@]}" -gt 0 ]]; then
    echo "Error: staged libs exceed GitHub's 100 MiB push limit:" >&2
    printf '  %s\n' "${OVERSIZED[@]}" >&2
    echo "The mirror push would be rejected (GH001). Shrink the staticlib in rs/moq-ffi/build.sh (LTO)." >&2
    exit 1
fi

# --- 4. Minimal consumer-facing README rewrite ---
# The full developer README lives in the monorepo; the published module
# gets a thin orientation pointer.
cat >"$PKG_STAGE/README.md" <<EOF
# moq.dev/moq-ffi (Go module)

Generated raw Go bindings for [Media over QUIC](https://github.com/moq-dev/moq). Most callers want the ergonomic [moq.dev/moq](https://pkg.go.dev/moq.dev/moq) wrapper instead.

Source, issues, and pull requests live in [moq-dev/moq](https://github.com/moq-dev/moq); this module only carries released bindings and their native libraries.

## Install

\`\`\`bash
go get moq.dev/moq-ffi@v${VERSION}
\`\`\`

The module bundles prebuilt native libraries for \`linux/amd64\`, \`linux/arm64\`, \`darwin/arm64\` (\`libmoq_ffi.a\`), and \`windows/amd64\` (\`moq_ffi.lib\`); cgo selects the right one automatically.

See [moq-dev/moq/go/ffi/README.md](https://github.com/moq-dev/moq/blob/main/go/ffi/README.md) for usage and the release process.

Licensed under MIT OR Apache-2.0.
EOF

echo ""
echo "Staged: $PKG_STAGE"

if [[ "$ARCHIVE" == true ]]; then
    ARCHIVE_PATH="$OUTPUT_DIR/${PKG_NAME}.tar.gz"
    tar -czf "$ARCHIVE_PATH" -C "$OUTPUT_DIR" "$PKG_NAME"
    echo "Created: $ARCHIVE_PATH"
fi
