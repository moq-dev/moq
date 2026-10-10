#!/usr/bin/env bash
#
# Trigger the apt-repo and rpm-repo workflows for a given release tag, and the
# moq.sh deploy for a moq-cli tag.
# Called from each per-binary release workflow after `gh release create`,
# because release:published events created via GITHUB_TOKEN don't cascade
# to other workflows automatically.
#
# Required env:
#   GH_TOKEN    GitHub token with workflow:write
#   TAG         Release tag, e.g. moq-relay-v1.2.3

set -euo pipefail

TAG="${TAG:?TAG must be set}"

echo "Dispatching apt-repo.yml for tag $TAG..."
gh workflow run apt-repo.yml -f "tag=$TAG"

echo "Dispatching rpm-repo.yml for tag $TAG..."
gh workflow run rpm-repo.yml -f "tag=$TAG"

# moq.sh bakes in the newest moq-cli as its default version. Deploy from
# `release`, which holds the installer that ships.
if [[ "$TAG" == moq-cli-v* ]]; then
    echo "Dispatching moq-sh.yml on release..."
    gh workflow run moq-sh.yml --ref release
fi
