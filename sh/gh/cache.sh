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
just infra nix push nix-ci-moq-dev "$profile/devshell"

# Smoke's nix channel currently runs only on x86 Linux and builds main HEAD.
# Publish its runtime closures without making the dev shell wait on Rust.
system=$(nix eval --impure --raw --expr builtins.currentSystem)
if [[ "$system" == x86_64-linux ]]; then
    # These existing Crane outputs survive source-only changes. Publish them
    # first so a later package build can substitute its dependency layer.
    nix build .#moq-relay.cargoArtifacts .#moq.cargoArtifacts --out-link "$profile/deps" --print-build-logs
    just infra nix push nix-ci-moq-dev "$profile/deps" "$profile/deps-1"

    nix build .#moq-relay .#moq --out-link "$profile/runtime" --print-build-logs
    just infra nix push nix-ci-moq-dev "$profile/runtime" "$profile/runtime-1"
fi
