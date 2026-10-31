#!/usr/bin/env bash
#
# Trigger the apt-repo and rpm-repo workflows for a given release tag, the
# winget submission for a moq-cli or moq-relay tag, and the moq.sh deploy for a
# moq-cli tag.
# Called from each per-binary release workflow after `gh release create`,
# because release:published events created via GITHUB_TOKEN don't cascade
# to other workflows automatically.
#
# Every dispatch names its ref: their credentials live in the `publish`
# Environment, which admits `release` and the release tags but not `main`,
# where a dispatch without --ref would run. apt and rpm run on `release`, which
# serves every tag, including `moq-gst-v*`, which `publish` does not admit.
#
# Required env:
#   GH_TOKEN    GitHub token with workflow:write
#   TAG         Release tag, e.g. moq-relay-v1.2.3

set -euo pipefail

TAG="${TAG:?TAG must be set}"

echo "Dispatching apt-repo.yml on release for tag $TAG..."
gh workflow run apt-repo.yml --ref release -f "tag=$TAG"

echo "Dispatching rpm-repo.yml on release for tag $TAG..."
gh workflow run rpm-repo.yml --ref release -f "tag=$TAG"

# winget submits the tag it runs on, as its own run, so a failed submission
# leaves this release's run, and release-brew chained on it, green.
if [[ "$TAG" == moq-cli-v* || "$TAG" == moq-relay-v* ]]; then
    echo "Dispatching release-winget.yml on $TAG..."
    gh workflow run release-winget.yml --ref "$TAG"
fi

# moq.sh bakes in the newest moq-cli as its default version. Deploy from
# `release`, which holds the installer that ships.
if [[ "$TAG" == moq-cli-v* ]]; then
    echo "Dispatching moq-sh.yml on release..."
    gh workflow run moq-sh.yml --ref release
fi
