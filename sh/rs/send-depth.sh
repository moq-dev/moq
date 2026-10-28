#!/usr/bin/env bash
# Prove native downstream futures at the default recursion limit on nightly.
set -euo pipefail

send_rust_version=$(rustc --version)
if [[ "$send_rust_version" == *-nightly* ]]; then
    send_sysroot=$(rustc --print sysroot)
elif command -v rustup >/dev/null 2>&1; then
    send_sysroot=$(rustup run nightly rustc --print sysroot)
else
    echo "rs: nightly Rust is missing; select it or run rustup toolchain install nightly" >&2
    exit 1
fi

# Nix puts its pinned stable compiler first; cargo and rustc must both be nightly.
export PATH="$send_sysroot/bin:$PATH"
export RUSTC="$send_sysroot/bin/rustc"
export RUSTDOC="$send_sysroot/bin/rustdoc"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D recursion_depth_exceeding_limit"
rustc --version
cargo check --locked -p moq-net --test send
