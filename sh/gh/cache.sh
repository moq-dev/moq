#!/usr/bin/env bash
# Publish the dev shell's build closure, including its build-time inputs.
set -euo pipefail

: "${NIX_SIGNING_KEY:?not set; the job needs the cache-write Environment}"
: "${AWS_ACCESS_KEY_ID:?not set}"
: "${AWS_SECRET_ACCESS_KEY:?not set}"

profile=$(mktemp -d)
trap 'rm -rf "$profile"' EXIT

# Building the devShell derivation alone omits inputs that nix develop needs.
nix develop --profile "$profile/devshell" --command true
just infra nix push nix-ci-moq-dev "$profile/devshell"
