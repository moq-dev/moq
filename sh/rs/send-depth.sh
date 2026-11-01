#!/usr/bin/env bash
# Prove native downstream futures at the default recursion limit on the
# flake's pinned nightly, so only a pin bump changes the compiler it checks.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# The pin lives only in flake.nix. A host without Nix, such as Windows, skips
# the check like any other missing tool (see sh/dispatch.sh); CI_STRICT, which
# every CI job sets, fails instead, since there a skip would read as a pass.
if ! command -v nix >/dev/null 2>&1; then
    if [[ -n "${CI_STRICT:-}" ]]; then
        echo "rs: send-depth needs nix for the pinned nightly" >&2
        exit 1
    fi
    echo "rs: nix is missing; skipping send-depth, which needs the flake's pinned nightly" >&2
    exit 0
fi
send_sysroot=$(nix build --no-link --print-out-paths .#rust-nightly)

# Nix puts its pinned stable compiler first; cargo and rustc must both be nightly.
export PATH="$send_sysroot/bin:$PATH"
# The nightly's cargo now shadows the Cargo shim `mbx setup` installs, as the
# dev shell's stable one does; put the shim back in front so the build stays
# cached. mbx runs the next cargo on PATH, the nightly's (see flake.nix).
mbx_shim=$( (mbx setup --status 2>/dev/null || true) | sed -n '1s/.*: //p')
if [[ -x "$mbx_shim" ]]; then
    PATH="${mbx_shim%/*}:$PATH"
fi
export RUSTC="$send_sysroot/bin/rustc"
export RUSTDOC="$send_sysroot/bin/rustdoc"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D recursion_depth_exceeding_limit"
rustc --version
cargo --version
cargo check --locked -p moq-net --test send
