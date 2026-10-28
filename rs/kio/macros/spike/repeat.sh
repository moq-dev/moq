#!/usr/bin/env bash
# repeat.sh <checkout> <name>: moq-net alone, non-incremental, three times per mode.
set -eu
root=$1
name=$2
spike=$(cd "$(dirname "$0")" && pwd)
cargo=/nix/store/85lxjwg78jw6rf7pqkf3vcq6291nrlfh-cargo-1.98.1-x86_64-unknown-linux-gnu/bin/cargo
export RUSTC_WRAPPER=$spike/rustc-perf.sh CARGO_INCREMENTAL=0
cd "$root"
for mode in check build; do
	export CARGO_TARGET_DIR=$spike/target-$name-$mode
	"$cargo" "$mode" --locked -p moq-net --lib -q
	rm -f "$spike/$name-$mode-repeat.csv"
	for run in 1 2 3; do
		touch rs/moq-net/src/lib.rs
		PERF_LOG=$spike/$name-$mode-repeat.csv "$cargo" "$mode" --locked -p moq-net --lib -q
	done
done
