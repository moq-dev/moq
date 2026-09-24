#!/usr/bin/env bash
# Build a .deb or .rpm for moq-relay or moq-cli locally: sh/rs/package.sh CRATE deb|rpm
#
# nfpm comes from the dev shell. An .rpm built this way links against the
# host's glibc; CI builds in an AlmaLinux 9 container for broad compatibility.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

crate=${1:?usage: sh/rs/package.sh CRATE deb|rpm}
packager=${2:?usage: sh/rs/package.sh CRATE deb|rpm}

case "$crate" in
    moq-relay) bin=moq-relay ;;
    moq-cli) bin=moq ;;
    *)
        echo "Unknown crate: $crate (use moq-relay or moq-cli)" >&2
        exit 1
        ;;
esac
case "$packager" in
    deb)
        if command -v dpkg >/dev/null 2>&1; then
            arch=$(dpkg --print-architecture)
        else
            case "$(uname -m)" in
                x86_64) arch=amd64 ;;
                aarch64 | arm64) arch=arm64 ;;
                *)
                    echo "Cannot infer deb arch from host $(uname -m)" >&2
                    exit 1
                    ;;
            esac
        fi
        ;;
    rpm) arch=$(uname -m) ;;
    *)
        echo "Unknown packager: $packager (use deb or rpm)" >&2
        exit 1
        ;;
esac
version=$(grep -m1 '^version' "rs/$crate/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
cargo build --locked --release -p "$crate"
mkdir -p dist
VERSION="$version" ARCH="$arch" BINARY_PATH="target/release/$bin" \
    sh/rs/package-nfpm.sh "packaging/$crate/nfpm.yaml" "$packager" dist/
if [[ "$packager" == deb && -f "packaging/$crate/transition.yaml" ]]; then
    VERSION="$version" ARCH="$arch" \
        sh/rs/package-nfpm.sh "packaging/$crate/transition.yaml" deb dist/
fi
ls -1 dist/
