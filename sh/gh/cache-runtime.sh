#!/usr/bin/env bash
# Publish Smoke's runtime packages and their reusable dependency artifacts.
set -euo pipefail

: "${NIX_SIGNING_KEY:?not set; the job needs the cache-write Environment}"
: "${AWS_ACCESS_KEY_ID:?not set}"
: "${AWS_SECRET_ACCESS_KEY:?not set}"

roots=$(mktemp -d)
trap 'rm -rf "$roots"' EXIT

# Publish Crane's existing dependency outputs first so later package builds
# can substitute them. Their inputs include the patched kio sources.
nix build .#moq-relay.cargoArtifacts .#moq.cargoArtifacts --out-link "$roots/deps" --print-build-logs
just infra nix push nix-ci-moq-dev "$roots/deps" "$roots/deps-1"

nix build .#moq-relay .#moq --out-link "$roots/runtime" --print-build-logs
just infra nix push nix-ci-moq-dev "$roots/runtime" "$roots/runtime-1"
