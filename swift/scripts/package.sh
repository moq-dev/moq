#!/usr/bin/env bash
set -euo pipefail

# Assemble the moq-ffi Swift Package: bundle per-target static libs into
# an XCFramework, copy the uniffi-generated Swift source, rewrite the
# Package.swift binary URL+checksum, and tar the result.
#
# Designed to run after rs/moq-ffi/build.sh produces per-target outputs.
# Only macOS hosts can run this (xcodebuild is required).
#
# Usage:
#   swift/scripts/package.sh --version 0.0.0-dev --lib-dir dist --output dist
#
#   --version       Version baked into Package.swift.
#   --lib-dir       Directory containing per-target moq-ffi outputs.
#   --output        Destination directory for the .tar.gz + xcframework.zip.
#   --bindings-dir  Directory with uniffi-bindgen swift output (defaults to
#                   "$LIB_DIR/bindings").
#   --release-url   Release URL prefix used as the XCFramework download
#                   target. Defaults to the upstream GitHub Releases URL;
#                   override when publishing from a fork.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SWIFT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$SWIFT_DIR/.." && pwd)"

VERSION=""
LIB_DIR=""
OUTPUT_DIR=""
BINDINGS_DIR=""
RELEASE_URL_BASE="https://github.com/moq-dev/moq/releases/download"

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
        --output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --bindings-dir)
            BINDINGS_DIR="$2"
            shift 2
            ;;
        --release-url)
            RELEASE_URL_BASE="$2"
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

[[ -z "$VERSION" ]] && {
    echo "Error: --version is required" >&2
    exit 1
}
[[ -z "$LIB_DIR" ]] && {
    echo "Error: --lib-dir is required" >&2
    exit 1
}
[[ -z "$OUTPUT_DIR" ]] && OUTPUT_DIR="dist"
[[ -z "$BINDINGS_DIR" ]] && BINDINGS_DIR="$LIB_DIR/bindings"

[[ "$(uname)" == "Darwin" ]] || {
    echo "Error: package.sh requires macOS (xcodebuild)" >&2
    exit 1
}
command -v xcodebuild >/dev/null || {
    echo "Error: xcodebuild not found" >&2
    exit 1
}
command -v swift >/dev/null || {
    echo "Error: swift not found" >&2
    exit 1
}

mkdir -p "$OUTPUT_DIR"
# Normalize to an absolute path: later steps (zip, swift package
# compute-checksum) run from cd'd subshells, so a relative OUTPUT_DIR
# would resolve against the wrong cwd.
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

STAGING=$(mktemp -d)
trap 'rm -rf "$STAGING"' EXIT

# --- Headers (modulemap + .h) shared by all slices ---
HEADERS_DIR="$STAGING/headers"
mkdir -p "$HEADERS_DIR"
[[ -f "$BINDINGS_DIR/moqFFI.h" ]] || {
    echo "Error: missing $BINDINGS_DIR/moqFFI.h" >&2
    exit 1
}
[[ -f "$BINDINGS_DIR/moqFFI.modulemap" ]] || {
    echo "Error: missing $BINDINGS_DIR/moqFFI.modulemap" >&2
    exit 1
}
[[ -f "$BINDINGS_DIR/moq.swift" ]] || {
    echo "Error: missing $BINDINGS_DIR/moq.swift" >&2
    exit 1
}
cp "$BINDINGS_DIR/moqFFI.h" "$HEADERS_DIR/"
cp "$BINDINGS_DIR/moqFFI.modulemap" "$HEADERS_DIR/module.modulemap"

# --- Per-slice library prep ---
lib_for() {
    echo "$LIB_DIR/$1/libmoq_ffi.a"
}

ensure_lib() {
    local path
    path=$(lib_for "$1")
    [[ -f "$path" ]] || {
        echo "Error: missing static lib for $1 at $path" >&2
        exit 1
    }
    echo "$path"
}

IOS_DEVICE_LIB=$(ensure_lib "aarch64-apple-ios")
IOS_SIM_ARM64=$(ensure_lib "aarch64-apple-ios-sim")
IOS_SIM_X86_64=$(ensure_lib "x86_64-apple-ios")
MAC_UNIVERSAL=$(ensure_lib "universal-apple-darwin")

# Fat lib for iOS simulator (arm64 + x86_64).
IOS_SIM_FAT="$STAGING/libmoq_ffi-iossim.a"
lipo -create "$IOS_SIM_ARM64" "$IOS_SIM_X86_64" -output "$IOS_SIM_FAT"

# --- Build XCFramework ---
XCF="$STAGING/MoqFFI.xcframework"
xcodebuild -create-xcframework \
    -library "$IOS_DEVICE_LIB" -headers "$HEADERS_DIR" \
    -library "$IOS_SIM_FAT" -headers "$HEADERS_DIR" \
    -library "$MAC_UNIVERSAL" -headers "$HEADERS_DIR" \
    -output "$XCF"

# --- Zip and checksum the XCFramework ---
XCF_ZIP="$OUTPUT_DIR/MoqFFI.xcframework.zip"
rm -f "$XCF_ZIP"
(cd "$STAGING" && zip -r -q "$XCF_ZIP" "$(basename "$XCF")")

# Move/copy to absolute path before computing checksum (swift requires
# it to live in a package).
CHECKSUM=$(cd "$SWIFT_DIR" && swift package compute-checksum "$XCF_ZIP")
echo "XCFramework checksum: $CHECKSUM"

# --- Assemble Swift package staging dir ---
PKG_NAME="moq-ffi-${VERSION}-swift"
PKG_STAGE="$STAGING/$PKG_NAME"
mkdir -p "$PKG_STAGE/Sources/Moq" "$PKG_STAGE/Sources/MoqFFI" "$PKG_STAGE/Tests/MoqTests"

cp -R "$SWIFT_DIR/Sources/Moq/." "$PKG_STAGE/Sources/Moq/"
cp -R "$SWIFT_DIR/Tests/MoqTests/." "$PKG_STAGE/Tests/MoqTests/"
cp "$BINDINGS_DIR/moq.swift" "$PKG_STAGE/Sources/MoqFFI/Generated.swift"

# Dual-license files lifted from the workspace root so the mirror isn't
# licenseless. Both files are required by the MIT OR Apache-2.0 grant.
for license in LICENSE-MIT LICENSE-APACHE; do
    [[ -f "$WORKSPACE_DIR/$license" ]] || {
        echo "Error: missing $WORKSPACE_DIR/$license" >&2
        exit 1
    }
    cp "$WORKSPACE_DIR/$license" "$PKG_STAGE/$license"
done

# Minimal consumer-facing README. The full developer README lives in
# the monorepo; this one just orients a visitor to moq-dev/moq-swift.
cat >"$PKG_STAGE/README.md" <<EOF
# Moq (Swift Package)

Auto-generated mirror of the Swift package for [Media over QUIC](https://github.com/moq-dev/moq).

Source, issues, and pull requests live in [moq-dev/moq](https://github.com/moq-dev/moq); this repo only carries tagged Swift Package Manager releases.

## Install

\`\`\`swift
.package(url: "https://github.com/moq-dev/moq-swift", from: "${VERSION}"),
\`\`\`

The package depends on a prebuilt \`MoqFFI.xcframework\` attached to the matching [moq-ffi-v${VERSION}](https://github.com/moq-dev/moq/releases/tag/moq-ffi-v${VERSION}) release on the source repo.

See [moq-dev/moq/swift/README.md](https://github.com/moq-dev/moq/blob/main/swift/README.md) for usage, local development, and release process.

Licensed under MIT OR Apache-2.0.
EOF

# Generate Package.swift with the final URL+checksum from the release
# template. The working swift/Package.swift is intentionally the
# local-dev (path-based) form and is not used here.
TEMPLATE="$SWIFT_DIR/Package.swift.template"
[[ -f "$TEMPLATE" ]] || {
    echo "Error: missing $TEMPLATE" >&2
    exit 1
}
URL="${RELEASE_URL_BASE}/moq-ffi-v${VERSION}/MoqFFI.xcframework.zip"
# Token-based substitution: the template carries REPLACE_URL / REPLACE_VERSION
# / REPLACE_CHECKSUM placeholders, so editing the upstream URL in the template
# (or passing --release-url) never goes silently unsubstituted.
sed -e "s|REPLACE_URL|${URL}|g" \
    -e "s|REPLACE_VERSION|${VERSION}|g" \
    -e "s|REPLACE_CHECKSUM|${CHECKSUM}|g" \
    "$TEMPLATE" >"$PKG_STAGE/Package.swift"

# Fail loudly if any placeholder survived (e.g. someone renamed a token
# in the template without updating this script). Catching it here keeps
# a broken manifest from reaching the mirror.
if grep -q 'REPLACE_URL\|REPLACE_VERSION\|REPLACE_CHECKSUM' "$PKG_STAGE/Package.swift"; then
    echo "Error: unresolved REPLACE_* tokens in generated Package.swift" >&2
    grep -n 'REPLACE_URL\|REPLACE_VERSION\|REPLACE_CHECKSUM' "$PKG_STAGE/Package.swift" >&2
    exit 1
fi

# Cheap manifest sanity check: parse the generated Package.swift via the
# Swift toolchain. This runs even on PR dry-runs (where the live release
# asset doesn't exist yet) and catches syntax / API breakage in the
# template before it can reach the mirror.
(cd "$PKG_STAGE" && swift package dump-package >/dev/null)

# --- Archive ---
ARCHIVE="$OUTPUT_DIR/${PKG_NAME}.tar.gz"
tar -czf "$ARCHIVE" -C "$STAGING" "$PKG_NAME"
echo "Created: $ARCHIVE"
echo "Created: $XCF_ZIP"
