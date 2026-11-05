#!/usr/bin/env bash
# Publish what CI builds to run checks: the dev shell's build closure,
# including its build-time inputs, and @moq/libav with its sources.
set -euo pipefail

: "${NIX_SIGNING_KEY:?not set; the job needs the cache-write Environment}"
: "${AWS_ACCESS_KEY_ID:?not set}"
: "${AWS_SECRET_ACCESS_KEY:?not set}"

roots=$(mktemp -d)
trap 'rm -rf "$roots"' EXIT

# Building the devShell derivation alone omits inputs that nix develop needs.
nix develop --profile "$roots/devshell" --command true
just infra nix push nix-ci-moq-dev "$roots/devshell"

# `bun install` builds @moq/libav outside the dev shell (js/libav/build.ts).
# The package copies the wasm build's files rather than referencing them, so
# the wasm build goes up separately: a change to the package alone (a version
# bump, README or index.d.ts) still substitutes the ffmpeg compile. With the
# sources here too, a PR that changes the wasm build never downloads from the
# upstream mirrors.
nix build .#moq-libav .#moq-libav.wasm .#moq-libav.sources --out-link "$roots/libav"
just infra nix push nix-ci-moq-dev "$roots"/libav*
