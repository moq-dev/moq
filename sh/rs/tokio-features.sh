#!/usr/bin/env bash
# Compile moq-tokio by itself at the feature extremes and as a bare QUIC driver;
# --all also isolates each transport permutation. The default-feature
# build is already in the ordinary gate, which can hide missing feature gates
# through unification.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

all=0
if [[ $# == 1 && $1 == --all ]]; then
    all=1
elif [[ $# != 0 ]]; then
    echo 'usage: just rs tokio-features [--all]' >&2
    exit 2
fi

cd "$(git rev-parse --show-toplevel)"
# shellcheck source=sh/checks.sh
source "$(dirname "${BASH_SOURCE[0]}")/../checks.sh"

run cargo clippy --locked -p moq-tokio --no-default-features -- -D warnings
run cargo clippy --locked -p moq-tokio --all-features -- -D warnings

# The QUIC driver is public API for applications without a MoQ session, so its
# build must not reach moq-net, the MoQ backend's dependencies, or another QUIC stack.
driver=(--no-default-features --features "driver")
targets=(--tests)
if ((all)); then targets=(--all-targets); fi
run cargo clippy --locked -p moq-tokio "${targets[@]}" "${driver[@]}" -- -D warnings
driver_dependencies() {
    graph=$(cargo tree --locked -p moq-tokio -e normal --prefix none "${driver[@]}") || return
    if leaked=$(grep -E '^(moq-net|reqwest|notify|quinn[a-z-]*|noq[a-z-]*|web-transport[a-z-]*) ' <<<"$graph"); then
        echo "rs: moq-tokio's driver build must not depend on:" >&2
        sort -u <<<"$leaked" >&2
        return 1
    fi
}
run driver_dependencies
# Every isolated permutation, which `just rs features` asks for.
if ((all)); then
    for feature in net qmux quic tcp uds websocket; do
        run cargo clippy --locked -p moq-tokio --no-default-features --features "$feature" -- -D warnings
    done
fi

# Crypto support alone must not make a transport-less server load a QUIC identity.
run bash sh/rs/nextest.sh cargo nextest run --locked -p moq-tokio --lib \
    --no-default-features --features tcp \
    -E 'test(no_quic_backend_needs_no_served_identity)'
finish
