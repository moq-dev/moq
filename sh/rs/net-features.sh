#!/usr/bin/env bash
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

# Even the full matrix must keep this library build free of dev-dependencies.
if [[ $# != 0 && ! ($# == 1 && $1 == --all) ]]; then
    echo 'usage: just rs net-features [--all]' >&2
    exit 2
fi

# Build moq-net alone: a workspace build unifies its default features back in.
# Normal dependencies exclude the simulation executor and bench-only Tokio.
minimal=$(cargo tree --locked -p moq-net --no-default-features -e normal --prefix none)
if grep -qE '^tokio ' <<<"$minimal"; then
    echo "moq-net without async must not depend on a native runtime" >&2
    exit 1
fi

# Tests build dev-dependencies too. Reject any path that enables async before
# compiling them, so a green test run cannot hide a broken minimal build.
features=$(cargo tree --locked -p moq-net --no-default-features -e features -i moq-net)
if grep -qE 'moq-net feature "(async|default|fuzz)"' <<<"$features"; then
    echo "moq-net test dependencies must not enable async" >&2
    echo "$features" >&2
    exit 1
fi

cargo clippy --locked -p moq-net --no-default-features --lib -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked -p moq-net --no-default-features --no-deps
bash sh/rs/nextest.sh cargo nextest run --locked -p moq-net --no-default-features --tests
