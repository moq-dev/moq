#!/usr/bin/env bash
# Publish the packages Smoke and deploys pin, and the dependency crates they link.
set -euo pipefail

: "${NIX_SIGNING_KEY:?not set; the job needs the cache-write Environment}"
: "${AWS_ACCESS_KEY_ID:?not set}"
: "${AWS_SECRET_ACCESS_KEY:?not set}"

roots=$(mktemp -d)
trap 'rm -rf "$roots"' EXIT

# Publish the dependency crates first so a later build substitutes them even
# when a package's own crate fails.
nix build .#moq-relay.deps .#moq.deps .#moq-boy.deps --out-link "$roots/deps" --print-build-logs
just infra nix push nix-ci-moq-dev "$roots"/deps*

nix build .#moq-relay .#moq .#moq-boy --out-link "$roots/runtime" --print-build-logs
just infra nix push nix-ci-moq-dev "$roots"/runtime*

# `nix copy` skipped paths already uploaded, possibly weeks ago, so restart
# their age now, while this commit is the newest Smoke may pin. The deps roots
# hold every crate output, which no runtime closure references.
just infra nix roots nix-ci-moq-dev "${GITHUB_SHA:?not set}" runtime-x86_64-linux "$roots"/*
just infra nix refresh nix-ci-moq-dev "$GITHUB_SHA"
