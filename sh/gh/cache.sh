#!/usr/bin/env bash
# Publish the dev shell's build closure and Smoke's runtime packages.
set -euo pipefail

: "${NIX_SIGNING_KEY:?not set; the job needs the cache-write Environment}"
: "${AWS_ACCESS_KEY_ID:?not set}"
: "${AWS_SECRET_ACCESS_KEY:?not set}"

profile=$(mktemp -d)
trap 'rm -rf "$profile"' EXIT

# Building the devShell derivation alone omits inputs that nix develop needs.
nix develop --profile "$profile/devshell" --command true

# Smoke's nix channel builds main HEAD, so it needs these runtime closures
# even before the next release tag publishes them to the release bucket.
for package in moq-relay moq; do
    nix build ".#$package" --out-link "$profile/$package" --print-build-logs
done

just infra nix push nix-ci-moq-dev "$profile/devshell" "$profile/moq-relay" "$profile/moq"
