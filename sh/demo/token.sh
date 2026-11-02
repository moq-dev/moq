#!/usr/bin/env bash
# Sign the local development tokens (demo-web.jwt, demo-cli.jwt) with root.jwk
# in the current directory, skipping any that already exist.
#
# Usage: sh/demo/token.sh, from demo/relay.
set -euo pipefail
umask 077

if [ ! -f demo-web.jwt ]; then
    cargo run --quiet --bin moq -- auth sign --key root.jwk \
        --root demo --subscribe '**' --publish 'me/**' \
        >demo-web.jwt
fi

if [ ! -f demo-cli.jwt ]; then
    cargo run --quiet --bin moq -- auth sign --key root.jwk \
        --root demo --publish '**' \
        >demo-cli.jwt
fi
