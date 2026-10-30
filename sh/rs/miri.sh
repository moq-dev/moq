#!/usr/bin/env bash
# Run the hand-written in-memory unsafe under Miri: sh/rs/miri.sh [NEXTEST_ARGS...]
#
# Nightly only: Miri interprets every instruction, orders of magnitude slower than a
# native test, so the `miri` nextest profile gives each test longer. A test Miri cannot
# run (foreign calls, real sockets, the wall clock) carries
# `#[cfg_attr(miri, ignore = "...")]` rather than leaving a filter here.
#
# Two passes, by whether a test reaches crossbeam-epoch's global collector, which
# `kio::log` frees segments through:
#
# - strict: Stacked Borrows and the leak check.
# - epoch: Tree Borrows, because the collector's intrusive list breaks Stacked Borrows
#   (https://github.com/crossbeam-rs/crossbeam/issues/545), and no leak check, because
#   the global collector never runs at exit, so whatever it still holds then leaks.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if [[ "$(rustc --version)" == *-nightly* ]]; then
    sysroot=$(rustc --print sysroot)
elif command -v rustup >/dev/null 2>&1; then
    sysroot=$(rustup run nightly rustc --print sysroot)
else
    echo "rs: nightly Rust is missing; select it or run rustup toolchain install nightly --component miri" >&2
    exit 1
fi

# Nix puts its pinned stable compiler first; cargo and rustc must both be nightly.
export PATH="$sysroot/bin:$PATH"
if ! cargo miri --version >/dev/null 2>&1; then
    echo "rs: Miri is missing: rustup component add --toolchain nightly miri" >&2
    exit 1
fi
rustc --version

run() {
    cargo miri nextest run --locked --profile miri --no-fail-fast "$@"
}

run -p kio -p moq-net --lib \
    -E '(package(kio) & not test(/^log::/)) | (package(moq-net) & (test(/^model::frame::/) | test(/^path::/)))' "$@"

MIRIFLAGS="${MIRIFLAGS:+$MIRIFLAGS }-Zmiri-tree-borrows -Zmiri-ignore-leaks" run -p kio -p moq-net --lib \
    -E '(package(kio) & test(/^log::/)) | (package(moq-net) & test(/^model::group::/))' "$@"
