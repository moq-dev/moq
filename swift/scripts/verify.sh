#!/usr/bin/env bash
set -euo pipefail

# Smoke-test a staged Swift package by building a throwaway SPM consumer
# project that depends on it via `.package(path:)`. Runs `swift package
# resolve` (downloads MoqFFI.xcframework.zip and verifies its SHA-256
# against the manifest's checksum) and `swift build` (compiles + links
# against the host slice of the xcframework).
#
# This catches a class of release regression where the staged
# Package.swift looks textually fine but SPM cannot actually resolve it.
# Used by release-swift.yml as a gate *before* the mirror push, so a
# broken manifest never reaches consumers.
#
# Usage:
#   swift/scripts/verify.sh --staged-dir <path>
#   swift/scripts/verify.sh --tarball <path/to/moq-ffi-X.Y.Z-swift.tar.gz>
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
    # The tarball wraps a single top-level moq-ffi-${VERSION}-swift dir.
    extracted=("$WORK"/moq-ffi-*-swift)
    [[ ${#extracted[@]} -eq 1 && -d "${extracted[0]}" ]] || {
        echo "Error: expected exactly one moq-ffi-*-swift dir in tarball, got: ${extracted[*]}" >&2
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

echo "verify: staged package at $STAGED_DIR"
echo "verify: --- Package.swift ---"
cat "$STAGED_DIR/Package.swift"
echo "verify: ---"

# SPM derives a path-based package's identity from the final path
# component, not from the manifest's `name:` field. Expose the staged
# dir under the published mirror name so the smoke project's
# `.product(package:)` reference matches the identity real consumers
# see when depending on github.com/moq-dev/moq-swift.
PKG_IDENTITY="moq-swift"
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
            dependencies: [.product(name: "Moq", package: "$PKG_IDENTITY")],
            path: "Sources/Smoke"
        ),
    ]
)
EOF

cat >"$SMOKE/Sources/Smoke/main.swift" <<'EOF'
import Moq
// Verify that the binary target's symbols are linkable, not just resolvable.
print("moq-swift verify ok")
EOF

cd "$SMOKE"
echo "verify: swift package resolve"
swift package resolve
echo "verify: swift build"
swift build
echo "verify: ok"
