#!/usr/bin/env bash
# llvm-lines.sh <checkout> <name>: cargo llvm-lines for moq-net's lib.
set -eu
spike=$(cd "$(dirname "$0")" && pwd)
export CARGO=/nix/store/85lxjwg78jw6rf7pqkf3vcq6291nrlfh-cargo-1.98.1-x86_64-unknown-linux-gnu/bin/cargo
export CARGO_TARGET_DIR=$spike/target-$2-lines
cd "$1"
cargo-llvm-lines llvm-lines --locked -p moq-net --lib > "$spike/$2-llvm-lines.txt"
rm -rf "$CARGO_TARGET_DIR"
