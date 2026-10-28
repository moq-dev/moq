#!/usr/bin/env bash
# quick.sh <style> <shape> <count> <mode> <log>: three clean (non-incremental) compiles of the synth crate.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
spike=$(dirname "$here")
cargo=/nix/store/85lxjwg78jw6rf7pqkf3vcq6291nrlfh-cargo-1.98.1-x86_64-unknown-linux-gnu/bin/cargo
export CARGO_TARGET_DIR=$spike/target-synth CARGO_INCREMENTAL=0
cd "$here"
python3 gen.py "$1" "$2" "$3"
"$cargo" "$4" -q --lib
rm -f "$5"
for run in 1 2 3; do
	touch src/lib.rs
	PERF_LOG=$5 RUSTC_WRAPPER=$spike/rustc-perf.sh "$cargo" "$4" -q --lib
done
