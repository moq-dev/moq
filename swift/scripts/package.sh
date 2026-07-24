#!/usr/bin/env bash
set -euo pipefail

# Assemble the ergonomic `Moq` Swift Package: stage the wrapper sources + tests,
# rewrite Package.swift from Package.swift.template (substituting the moq-ffi
# dependency pin), and tar the result. This package is published to
# moq-dev/moq-swift and versioned independently of the moq-ffi crate
# (see swift/VERSION).
#
# The wrapper is pure Swift: no native build, no xcframework. The prebuilt
# bindings come from the moq-ffi Swift package (package-ffi.sh), which this
# package depends on at .upToNextMinor(from: <moq-ffi crate version>).
#
# Usage:
#   swift/scripts/package.sh --version 0.3.0 --output dist
#
#   --version       Wrapper version (from swift/VERSION). Names the tarball only;
#                   the SPM version comes from the mirror's git tag.
#   --ffi-version   moq-ffi crate version to pin. Defaults to the version in
#                   rs/moq-ffi/Cargo.toml.
#   --output        Destination directory for the .tar.gz.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SWIFT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$SWIFT_DIR/.." && pwd)"

VERSION=""
FFI_VERSION=""
OUTPUT_DIR=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --version)
            VERSION="$2"
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
[[ -z "$OUTPUT_DIR" ]] && OUTPUT_DIR="dist"

# Default the moq-ffi pin to the crate's current version. This is the SPM analog
# of py's `moq-ffi ~= 0.2.x`: the wrapper floats to the latest compatible patch.
if [[ -z "$FFI_VERSION" ]]; then
    FFI_VERSION=$(grep '^version' "$WORKSPACE_DIR/rs/moq-ffi/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
    [[ -n "$FFI_VERSION" ]] || {
        echo "Error: could not read moq-ffi version from rs/moq-ffi/Cargo.toml" >&2
        exit 1
    }
    echo "moq-ffi pin (from Cargo.toml): $FFI_VERSION"
fi

command -v swift >/dev/null || {
    echo "Error: swift not found" >&2
    exit 1
}

# `nix develop` on Darwin exports SDKROOT/DEVELOPER_DIR pointing at the
# nixpkgs-bundled apple-sdk, which the Xcode swift in PATH refuses to load.
# Drop those so swift falls back to `xcrun --show-sdk-path`. No-op in CI
# (macOS runners don't set a nix SDKROOT). Mirrors check.sh.
xcode_sdk_env=()
if [[ "${SDKROOT:-}" == /nix/store/* ]]; then
    xcode_sdk_env+=(-u SDKROOT)
fi
if [[ "${DEVELOPER_DIR:-}" == /nix/store/* ]]; then
    xcode_sdk_env+=(-u DEVELOPER_DIR)
fi

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

STAGING=$(mktemp -d)
trap 'rm -rf "$STAGING"' EXIT

PKG_NAME="moq-${VERSION}-swift"
PKG_STAGE="$STAGING/$PKG_NAME"
mkdir -p "$PKG_STAGE/Sources/Moq" "$PKG_STAGE/Tests/MoqTests"

cp -R "$SWIFT_DIR/Sources/Moq/." "$PKG_STAGE/Sources/Moq/"
cp -R "$SWIFT_DIR/Tests/MoqTests/." "$PKG_STAGE/Tests/MoqTests/"

# Swift Package Index reads .spi.yml from the package root to build and host the
# DocC docs. Ship it in the mirror so SPI documents the tagged release.
cp "$SWIFT_DIR/.spi.yml" "$PKG_STAGE/.spi.yml"

# Dual-license files lifted from the workspace root so the mirror isn't
# licenseless. Both files are required by the MIT OR Apache-2.0 grant.
for license in LICENSE-MIT LICENSE-APACHE; do
    [[ -f "$WORKSPACE_DIR/$license" ]] || {
        echo "Error: missing $WORKSPACE_DIR/$license" >&2
        exit 1
    }
    cp "$WORKSPACE_DIR/$license" "$PKG_STAGE/$license"
done

# Minimal consumer-facing README. The full developer README lives in the
# monorepo; this one just orients a visitor to moq-dev/moq-swift.
cat >"$PKG_STAGE/README.md" <<EOF
# Moq (Swift Package)

Ergonomic Swift wrapper for [Media over QUIC](https://github.com/moq-dev/moq):
async/await, \`AsyncSequence\` streams, and Swift-native names over the raw
[moq-ffi](https://github.com/moq-dev/moq-swift-ffi) bindings.

Auto-generated mirror; source, issues, and pull requests live in
[moq-dev/moq](https://github.com/moq-dev/moq). This repo only carries tagged
Swift Package Manager releases, versioned independently of the moq-ffi crate.

## Install

\`\`\`swift
.package(url: "https://github.com/moq-dev/moq-swift", from: "${VERSION}"),
\`\`\`

The raw \`MoqFFI\` bindings (and the prebuilt XCFramework) are pulled in
transitively from [moq-dev/moq-swift-ffi](https://github.com/moq-dev/moq-swift-ffi).

See [moq-dev/moq/swift/README.md](https://github.com/moq-dev/moq/blob/main/swift/README.md)
for usage, local development, and the release process.

Licensed under MIT OR Apache-2.0.
EOF

# Generate Package.swift with the moq-ffi pin from the release template. The
# working swift/Package.swift is intentionally the monolithic local-dev
# (path-based) form and is not used here.
TEMPLATE="$SWIFT_DIR/Package.swift.template"
[[ -f "$TEMPLATE" ]] || {
    echo "Error: missing $TEMPLATE" >&2
    exit 1
}
sed -e "s|REPLACE_FFI_VERSION|${FFI_VERSION}|g" \
    "$TEMPLATE" >"$PKG_STAGE/Package.swift"

if grep -q 'REPLACE_FFI_VERSION' "$PKG_STAGE/Package.swift"; then
    echo "Error: unresolved REPLACE_FFI_VERSION token in generated Package.swift" >&2
    exit 1
fi

# Cheap manifest sanity check: parse the generated Package.swift via the
# Swift toolchain. Catches syntax / API breakage in the template before it
# can reach the mirror. Does not resolve the moq-ffi dependency.
(cd "$PKG_STAGE" && env ${xcode_sdk_env[@]+"${xcode_sdk_env[@]}"} swift package dump-package >/dev/null)

ARCHIVE="$OUTPUT_DIR/${PKG_NAME}.tar.gz"
tar -czf "$ARCHIVE" -C "$STAGING" "$PKG_NAME"
echo "Created: $ARCHIVE"
