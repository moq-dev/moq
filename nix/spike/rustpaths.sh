#!/usr/bin/env bash
# rustpaths.sh DRV: the Rust build outputs (crane layers and packages, or
# per-crate derivations) in DRV's build closure, valid or not.
set -euo pipefail
nix-store -qR "$1" | grep -E -- '-(rust_[^/]*|moq-(relay|cli)(-deps)?-[0-9][^/]*|moq-spike-[a-z0-9]+)\.drv$' \
  | xargs nix-store -q --outputs
