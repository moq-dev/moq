#!/usr/bin/env bash
set -euo pipefail

# Smoke-test a staged MoqFFI Swift package by building a throwaway SPM consumer
# that depends on it via `.package(path:)`. Runs `swift package resolve`
# (downloads MoqFFI.xcframework.zip and verifies its SHA-256 against the
# manifest's checksum) and `swift build` (compiles + links against the host
# slice of the xcframework).
#
# This catches a class of release regression where the staged Package.swift
# looks textually fine but SPM cannot actually resolve it. Used by
# release-swift-ffi.yml as a gate *before* the mirror push, so a broken manifest
# never reaches consumers.
#
# Usage:
#   swift/scripts/verify-ffi.sh --staged-dir <path>
#   swift/scripts/verify-ffi.sh --tarball <path/to/moq-ffi-X.Y.Z-swift-ffi.tar.gz>
#
#   Exactly one of --staged-dir / --tarball must be passed.

STAGED_DIR=""
TARBALL=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --staged-dir)
            STAGED_DIR="$2"
            shift 2
            ;;
        --tarball)
            TARBALL="$2"
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

if [[ -n "$STAGED_DIR" && -n "$TARBALL" ]]; then
    echo "Error: pass exactly one of --staged-dir or --tarball" >&2
    exit 1
fi
if [[ -z "$STAGED_DIR" && -z "$TARBALL" ]]; then
    echo "Error: --staged-dir or --tarball is required" >&2
    exit 1
fi

command -v swift >/dev/null || {
    echo "Error: swift not found on PATH" >&2
    exit 1
}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

if [[ -n "$TARBALL" ]]; then
    [[ -f "$TARBALL" ]] || {
        echo "Error: tarball not found: $TARBALL" >&2
        exit 1
    }
    tar -xzf "$TARBALL" -C "$WORK"
    # The tarball wraps a single top-level moq-ffi-${VERSION}-swift-ffi dir.
    extracted=("$WORK"/moq-ffi-*-swift-ffi)
    [[ ${#extracted[@]} -eq 1 && -d "${extracted[0]}" ]] || {
        echo "Error: expected exactly one moq-ffi-*-swift-ffi dir in tarball, got: ${extracted[*]}" >&2
        exit 1
    }
    STAGED_DIR="${extracted[0]}"
fi

# Resolve to absolute path; SPM resolves relative .package(path:) against
# the consumer manifest, which lives under $WORK below.
STAGED_DIR=$(cd "$STAGED_DIR" && pwd)
[[ -f "$STAGED_DIR/Package.swift" ]] || {
    echo "Error: $STAGED_DIR/Package.swift missing" >&2
    exit 1
}

echo "verify-ffi: staged package at $STAGED_DIR"
echo "verify-ffi: --- Package.swift ---"
cat "$STAGED_DIR/Package.swift"
echo "verify-ffi: ---"

# SPM derives a path-based package's identity from the final path component,
# not from the manifest's `name:` field. Expose the staged dir under the
# published mirror name so the smoke project's `.product(package:)` reference
# matches the identity real consumers see.
PKG_IDENTITY="moq-swift-ffi"
PKG_LINK="$WORK/$PKG_IDENTITY"
ln -s "$STAGED_DIR" "$PKG_LINK"

SMOKE="$WORK/smoke"
mkdir -p "$SMOKE/Sources/Smoke"

cat >"$SMOKE/Package.swift" <<EOF
// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Smoke",
    platforms: [.iOS(.v15), .macOS(.v12)],
    dependencies: [
        .package(path: "$PKG_LINK"),
    ],
    targets: [
        .executableTarget(
            name: "Smoke",
            dependencies: [.product(name: "MoqFFI", package: "$PKG_IDENTITY")],
            path: "Sources/Smoke"
        ),
    ]
)
EOF

cat >"$SMOKE/Sources/Smoke/main.swift" <<'EOF'
import MoqFFI
// Verify that the binary target's symbols are linkable, not just resolvable.
let client = MoqClient()
client.cancel()
print("moq-swift-ffi verify ok")
EOF

cd "$SMOKE"
echo "verify-ffi: swift package resolve"
swift package resolve
echo "verify-ffi: swift build"
swift build
echo "verify-ffi: ok"
