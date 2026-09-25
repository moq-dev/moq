#!/usr/bin/env bash
# Coverage-guided fuzzing of the moq-net wire codecs: sh/rs/fuzz.sh TARGET [ARGS...]
#
# TARGET is lite, ietf, varint, or path; ARGS pass to `cargo fuzz run`, so
# `just rs fuzz lite -- -max_total_time=300` bounds a run. See
# rs/moq-net/fuzz/README.md.
#
# Seeds are regenerated first so the corpus follows the dispatch rather than a
# stale run, and libFuzzer writes what it discovers to the FIRST corpus
# directory, which is why the generated seeds are passed second and stay
# read-only.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

target=${1:?usage: sh/rs/fuzz.sh TARGET [ARGS...]}
shift

if ! command -v cargo-fuzz >/dev/null 2>&1; then
    echo "rs: cargo-fuzz is missing: cargo install --locked cargo-fuzz" >&2
    exit 1
fi

if ! rustup run nightly rustc --version >/dev/null 2>&1; then
    echo "rs: the nightly toolchain is missing: rustup toolchain install nightly" >&2
    exit 1
fi

cargo run --locked -q -p moq-net --features fuzz --example fuzz-seeds -- rs/moq-net/fuzz/seeds
mkdir -p rs/moq-net/fuzz/corpus/"$target"

# Nightly by PATH rather than by `cargo +nightly`: the dev shell puts a
# Nix-provided cargo ahead of the rustup shim, so the `+` form is not
# understood, and cargo-fuzz shells out to a bare `cargo` and `rustc` anyway.
# Both have to be the nightly ones or the sanitizer flags are rejected.
sysroot=$(rustup run nightly rustc --print sysroot)
export PATH="$sysroot/bin:$PATH"

cargo fuzz run --fuzz-dir rs/moq-net/fuzz "$target" \
    rs/moq-net/fuzz/corpus/"$target" rs/moq-net/fuzz/seeds/"$target" "$@"
