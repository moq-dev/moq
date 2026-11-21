#!/usr/bin/env bash
# Run clippy for macOS and Windows from any host, so a platform-gated compile
# error or unfulfilled lint expectation fails the gate rather than the nightly
# native lanes. The selection comes from `sh/rs/select.sh cross`; the targets
# are the Mac runner's and the one release-binary.yml ships for Windows.
#
# Usage: sh/rs/cross-lint.sh CARGO_ARGS...
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

targets=(aarch64-apple-darwin x86_64-pc-windows-msvc)

# The dev shell's compiler plus those targets' std, pinned in flake.nix.
sysroot=$(nix build --no-link --print-out-paths .#rust-cross)
export PATH="$sysroot/bin:$PATH"
# Put the Cargo shim `mbx setup` installs back in front, as the dev shell
# does, so the build stays cached.
mbx_shim=$( (mbx setup --status 2>/dev/null || true) | sed -n '1s/.*: //p')
if [[ -x "$mbx_shim" ]]; then
    PATH="${mbx_shim%/*}:$PATH"
fi
export RUSTC="$sysroot/bin/rustc"

# A build script's C and C++ for a foreign target go through zig, which
# carries every target's libc headers.
for target in "${targets[@]}"; do
    var=${target//-/_}
    export "CC_$var=$PWD/sh/rs/zig-cc.sh" "CXX_$var=$PWD/sh/rs/zig-c++.sh" "AR_$var=$PWD/sh/rs/zig-ar.sh"
done

# Both targets in one build share its host build scripts and proc macros.
cargo clippy --locked "${targets[@]/#/--target=}" --all-targets "$@" -- -D warnings
