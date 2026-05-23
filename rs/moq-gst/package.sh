#!/usr/bin/env bash
#
# Build and package the moq-gst GStreamer plugin as a .deb or .rpm.
# Unlike build.sh (which uses Nix for a reproducible tarball), this script
# compiles via the host distro's cargo and links against the system's
# libgstreamer1.0-dev / gstreamer1-devel so the resulting .so is symbol-
# compatible with the gstreamer the user's package manager will install.
#
# Usage:
#   ./package.sh --packager {deb|rpm} --version <semver> --arch <pkg-arch>
#               [--target <rust-target>] [--output <dir>]
#
# The default target is the host triple. pkg-arch follows nfpm conventions
# (amd64/arm64 for .deb; x86_64/aarch64 for .rpm).
#
# Required tools on the build host:
#   .deb (Ubuntu 22.04+ recommended):
#     apt-get install -y build-essential pkg-config libgstreamer1.0-dev \
#                        libgstreamer-plugins-base1.0-dev
#     plus rustup. nfpm comes from the flake's dev shell.
#   .rpm (AlmaLinux 9 / RHEL 9 / Rocky 9 recommended):
#     dnf install -y gcc pkgconf-pkg-config gstreamer1-devel \
#                    gstreamer1-plugins-base-devel
#     plus rustup. nfpm comes from the flake's dev shell.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

PACKAGER=""
VERSION=""
PKG_ARCH=""
RUST_TARGET=""
OUTPUT_DIR="$WORKSPACE_DIR/dist"

require_value() {
    if [[ $# -lt 2 || -z "${2:-}" ]]; then
        echo "Missing value for $1" >&2
        exit 1
    fi
}

while [[ $# -gt 0 ]]; do
    case $1 in
        --packager) require_value "$@"; PACKAGER="$2"; shift 2 ;;
        --version)  require_value "$@"; VERSION="$2";  shift 2 ;;
        --arch)     require_value "$@"; PKG_ARCH="$2"; shift 2 ;;
        --target)   require_value "$@"; RUST_TARGET="$2"; shift 2 ;;
        --output)   require_value "$@"; OUTPUT_DIR="$2"; shift 2 ;;
        -h|--help)
            sed -n '2,/^set -euo pipefail/p' "$0" | sed 's/^# //;s/^#//' | head -n -1
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

if [[ "$PACKAGER" != "deb" && "$PACKAGER" != "rpm" ]]; then
    echo "Error: --packager must be 'deb' or 'rpm'" >&2
    exit 1
fi

if [[ -z "$VERSION" ]]; then
    VERSION=$(grep '^version' "$SCRIPT_DIR/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
    echo "Detected version: $VERSION"
fi

if [[ -z "$RUST_TARGET" ]]; then
    RUST_TARGET=$(rustc -vV | grep host | cut -d' ' -f2)
    echo "Detected rust target: $RUST_TARGET"
fi

if [[ -z "$PKG_ARCH" ]]; then
    case "$RUST_TARGET" in
        x86_64-unknown-linux-*)
            PKG_ARCH=$([[ "$PACKAGER" == "deb" ]] && echo "amd64" || echo "x86_64") ;;
        aarch64-unknown-linux-*)
            PKG_ARCH=$([[ "$PACKAGER" == "deb" ]] && echo "arm64" || echo "aarch64") ;;
        *)
            echo "Cannot derive --arch from target $RUST_TARGET; please pass it explicitly." >&2
            exit 1 ;;
    esac
    echo "Derived pkg arch: $PKG_ARCH"
fi

# Plugin install directory varies by distro+arch.
case "$PACKAGER:$PKG_ARCH" in
    deb:amd64)       PLUGIN_DIR="/usr/lib/x86_64-linux-gnu/gstreamer-1.0" ;;
    deb:arm64)       PLUGIN_DIR="/usr/lib/aarch64-linux-gnu/gstreamer-1.0" ;;
    rpm:x86_64)      PLUGIN_DIR="/usr/lib64/gstreamer-1.0" ;;
    rpm:aarch64)     PLUGIN_DIR="/usr/lib64/gstreamer-1.0" ;;
    *)
        echo "Unsupported --packager/--arch combination: $PACKAGER/$PKG_ARCH" >&2
        exit 1 ;;
esac

echo ">> Building moq-gst for $RUST_TARGET against system gstreamer..."
(
    cd "$WORKSPACE_DIR"
    cargo build --release --target "$RUST_TARGET" -p moq-gst
)

BUILT_SO="$WORKSPACE_DIR/target/$RUST_TARGET/release/libgstmoq.so"
if [[ ! -f "$BUILT_SO" ]]; then
    echo "Error: expected $BUILT_SO after cargo build, none found." >&2
    exit 1
fi

# nfpm config is shared across packagers; we name the package per ecosystem
# convention (gstreamer1.0-moq for .deb, gstreamer1-moq for .rpm).
case "$PACKAGER" in
    deb) PKG_NAME="gstreamer1.0-moq" ;;
    rpm) PKG_NAME="gstreamer1-moq" ;;
esac

mkdir -p "$OUTPUT_DIR"

echo ">> Running nfpm ($PACKAGER)..."
export VERSION ARCH="$PKG_ARCH" PKG_NAME PLUGIN_PATH="$BUILT_SO" PLUGIN_DIR
nfpm pkg \
    --packager "$PACKAGER" \
    --config "$WORKSPACE_DIR/packaging/moq-gst/nfpm.yaml" \
    --target "$OUTPUT_DIR/"

echo ">> Done. Artifacts in: $OUTPUT_DIR"
ls -1 "$OUTPUT_DIR"
