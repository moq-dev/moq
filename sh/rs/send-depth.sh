#!/usr/bin/env bash
# Prove native downstream futures at the default recursion limit on the
# flake's pinned nightly, so only a pin bump changes the compiler it checks.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
send_sysroot=$(nix build --no-link --print-out-paths .#rust-nightly)

# Nix puts its pinned stable compiler first; cargo and rustc must both be nightly.
export PATH="$send_sysroot/bin:$PATH"
export RUSTC="$send_sysroot/bin/rustc"
export RUSTDOC="$send_sysroot/bin/rustdoc"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D recursion_depth_exceeding_limit"
rustc --version
cargo check --locked -p moq-net --test send
