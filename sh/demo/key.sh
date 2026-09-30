#!/usr/bin/env bash
# Generate the relay's signing key (root.jwk) in the current directory, unless
# it already exists. A new key invalidates every token, so they are removed.
#
# Usage: sh/demo/key.sh, from demo/relay.
set -euo pipefail
umask 077

[ -f root.jwk ] && exit 0
rm -f ./*.jwt
cargo run --bin moq -- auth generate --out root.jwk
