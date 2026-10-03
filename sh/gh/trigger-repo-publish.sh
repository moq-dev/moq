#!/usr/bin/env bash
#
# Trigger the apt-repo and rpm-repo workflows for a given release tag.
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
