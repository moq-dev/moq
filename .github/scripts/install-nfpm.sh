#!/usr/bin/env bash
#
# Install a pinned release of nfpm (https://nfpm.goreleaser.com) into
# /usr/local/bin. Detects host arch automatically.
#
# Bumping: pick a release from https://github.com/goreleaser/nfpm/releases
# and update NFPM_VERSION below.

set -euo pipefail

NFPM_VERSION="2.41.3"

case "$(uname -m)" in
    x86_64)         NFPM_ARCH="x86_64" ;;
    aarch64|arm64)  NFPM_ARCH="arm64" ;;
    *)
        echo "Unsupported architecture: $(uname -m)" >&2
        exit 1 ;;
esac

URL="https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}/nfpm_${NFPM_VERSION}_Linux_${NFPM_ARCH}.tar.gz"

echo "Downloading nfpm ${NFPM_VERSION} (${NFPM_ARCH})..."
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
curl -sSL "$URL" | tar -xz -C "$TMP" nfpm
sudo install -m 0755 "$TMP/nfpm" /usr/local/bin/nfpm
nfpm --version
