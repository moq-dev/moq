#!/usr/bin/env bash
# Compile moq-tokio by itself at the feature extremes; --all also isolates each
# crypto and transport permutation. The default-feature build is already in the
# ordinary gate, which can hide missing feature gates through unification.
set -euo pipefail

all=0
if [[ $# == 1 && $1 == --all ]]; then
    all=1
elif [[ $# != 0 ]]; then
    echo 'usage: just rs tokio-features [--all]' >&2
    exit 2
fi

cd "$(git rev-parse --show-toplevel)"

cargo clippy --locked -p moq-tokio --no-default-features -- -D warnings
cargo clippy --locked -p moq-tokio --all-features -- -D warnings
# Only a feature-owner edit needs every isolated permutation on a PR. Shared
# dependencies still compile at both extremes; Nightly runs every permutation.
if ((all)); then
    cargo clippy --locked -p moq-tokio --no-default-features --features quic,aws-lc-rs -- -D warnings
    cargo clippy --locked -p moq-tokio --no-default-features --features quic,ring -- -D warnings

    for feature in aws-lc-rs ring tcp uds websocket; do
        cargo clippy --locked -p moq-tokio --no-default-features --features "$feature" -- -D warnings
    done
fi

# shellcheck disable=SC2016 # the backticks are literal, as rustc prints them
want='a rustls QUIC backend requires a crypto provider: enable either the `aws-lc-rs` or `ring` feature'
if output=$(cargo check --locked -p moq-tokio --no-default-features --features quic 2>&1); then
    echo "rs: moq-tokio with only quic should reject its missing crypto provider" >&2
    exit 1
fi
if ! grep -qF "$want" <<<"$output"; then
    echo "rs: moq-tokio with only quic should fail on: $want" >&2
    echo "$output" | grep -E '^error' >&2 || true
    exit 1
fi

# Crypto support alone must not make a transport-less server load a QUIC identity.
bash sh/rs/nextest.sh cargo nextest run --locked -p moq-tokio --lib \
    --no-default-features --features tcp,aws-lc-rs \
    -E 'test(no_quic_backend_needs_no_served_identity)'
