#!/usr/bin/env bash
# Print the name of every `*-fuzz` workspace member, one per line.
#
# The fuzz crates are members only to share the root lockfile; libFuzzer needs nightly,
# so every `--workspace` run excludes them. Deriving the list means a new one is
# excluded without editing each caller (rs/justfile cannot call this, so a test keeps
# its `no_fuzz` equal to it).
set -euo pipefail

cargo metadata --locked --format-version 1 --no-deps |
    jq -r '.packages[].name | select(endswith("-fuzz"))' |
    LC_ALL=C sort
