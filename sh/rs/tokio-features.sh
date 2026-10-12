#!/usr/bin/env bash
# Compile moq-tokio by itself at the feature extremes and with each crypto
# provider. Its default-feature build is already part of the ordinary clippy
# pass; this catches what workspace feature unification hides.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

cargo clippy --locked -p moq-tokio --no-default-features -- -D warnings
cargo clippy --locked -p moq-tokio --all-features -- -D warnings
cargo clippy --locked -p moq-tokio --no-default-features --features noq,aws-lc-rs -- -D warnings
cargo clippy --locked -p moq-tokio --no-default-features --features noq,ring -- -D warnings

for feature in aws-lc-rs ring tcp uds websocket; do
    cargo clippy --locked -p moq-tokio --no-default-features --features "$feature" -- -D warnings
done

# shellcheck disable=SC2016 # the backticks are literal, as rustc prints them
want='a rustls QUIC backend requires a crypto provider: enable either the `aws-lc-rs` or `ring` feature'
if output=$(cargo check --locked -p moq-tokio --no-default-features --features noq 2>&1); then
    echo "rs: moq-tokio with only noq should reject its missing crypto provider" >&2
    exit 1
fi
if ! grep -qF "$want" <<<"$output"; then
    echo "rs: moq-tokio with only noq should fail on: $want" >&2
    echo "$output" | grep -E '^error' >&2 || true
    exit 1
fi

# Crypto support alone must not make a transport-less server load a QUIC identity.
bash sh/rs/nextest.sh cargo nextest run --locked -p moq-tokio --lib \
    --no-default-features --features tcp,aws-lc-rs \
    -E 'test(no_quic_backend_needs_no_served_identity)'
