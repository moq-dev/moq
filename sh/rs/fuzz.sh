#!/usr/bin/env bash
# Coverage-guided fuzzing: sh/rs/fuzz.sh TARGET [ARGS...]
#
# TARGET names a file in some crate's fuzz/fuzz_targets/, such as moq-net's lite or
# moq-quic's connection; names are unique across crates. ARGS pass to
# `cargo fuzz run`, so `just rs fuzz lite -- -max_total_time=300` bounds a run.
# See rs/moq-net/fuzz/README.md and rs/moq-quic/fuzz/README.md.
#
# Seeds are regenerated first so the corpus follows the code rather than a
# stale run, and libFuzzer writes what it discovers to the FIRST corpus
# directory, which is why the generated seeds are passed second and stay
# read-only.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

target=${1:?usage: sh/rs/fuzz.sh TARGET [ARGS...]}
shift

shopt -s nullglob
shims=(rs/*/fuzz/fuzz_targets/"$target".rs)
shopt -u nullglob
if [[ ${#shims[@]} -ne 1 ]]; then
    echo "rs: expected one fuzz target named $target, found ${#shims[@]}: ${shims[*]}" >&2
    exit 1
fi
# The crate directory is named after its crate.
crate=$(basename "$(dirname "$(dirname "$(dirname "${shims[0]}")")")")
dir=rs/$crate/fuzz

if ! command -v cargo-fuzz >/dev/null 2>&1; then
    echo "rs: cargo-fuzz is missing: cargo install --locked cargo-fuzz" >&2
    exit 1
fi

if ! rustup run nightly rustc --version >/dev/null 2>&1; then
    echo "rs: the nightly toolchain is missing: rustup toolchain install nightly" >&2
    exit 1
fi

cargo run --locked -q -p "$crate" --features fuzz --example fuzz-seeds -- "$dir"/seeds
mkdir -p "$dir"/corpus/"$target"

# Nightly by PATH rather than by `cargo +nightly`: the dev shell puts a
# Nix-provided cargo ahead of the rustup shim, so the `+` form is not
# understood, and cargo-fuzz shells out to a bare `cargo` and `rustc` anyway.
# Both have to be the nightly ones or the sanitizer flags are rejected.
sysroot=$(rustup run nightly rustc --print sysroot)
export PATH="$sysroot/bin:$PATH"

# Nightly checks the trait solver's recursion depth while compiling the model.
# Fail here before an overflow becomes a hard error on stable.
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D recursion_depth_exceeding_limit"

# Fat LTO drops sanitizer-coverage globals referenced by ASan. Keep symbol
# names too, so sanitizer and libFuzzer reports identify the crashing code.
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_STRIP=none

cargo fuzz run --fuzz-dir "$dir" "$target" \
    "$dir"/corpus/"$target" "$dir"/seeds/"$target" "$@"
