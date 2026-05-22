#!/usr/bin/env bash
set -euo pipefail

# Push the Swift Package contents to the moq-dev/moq-swift mirror repo
# on the matching moq-ffi-v$BUILD_VERSION tag. SPM consumers point at
# moq-dev/moq-swift instead of this repo because Package.swift must live
# at the root of the resolved tag.
#
# Required environment:
#   BUILD_VERSION       - version string (e.g. 0.2.10)
#   SWIFT_MIRROR_TOKEN  - PAT or GitHub App token with contents:write on moq-dev/moq-swift
#
# Expects the staged Swift package tarball under `swift-out/`.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

: "${BUILD_VERSION:?BUILD_VERSION is required}"
: "${SWIFT_MIRROR_TOKEN:?SWIFT_MIRROR_TOKEN is required}"

MIRROR_REPO="${SWIFT_MIRROR_REPO:-moq-dev/moq-swift}"
TAG="moq-ffi-v${BUILD_VERSION}"

# This script is intentionally a stub. Production wiring needs:
#   1. git clone https://x-access-token:$SWIFT_MIRROR_TOKEN@github.com/$MIRROR_REPO mirror
#   2. tar -xzf swift-out/moq-ffi-${BUILD_VERSION}-swift.tar.gz -C /tmp
#   3. rsync --delete /tmp/moq-ffi-${BUILD_VERSION}-swift/ mirror/
#   4. (cd mirror && git add -A && git commit -m "Release ${TAG}" && git tag ${TAG} && git push origin HEAD --tags)
#
# Wire those steps once PUBLISH_SPM is flipped on and SWIFT_MIRROR_TOKEN exists.
echo "publish-spm: stub. See swift/README.md for the deployment recipe."
exit 1
