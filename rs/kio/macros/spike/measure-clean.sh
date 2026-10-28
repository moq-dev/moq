#!/usr/bin/env bash
# measure-clean.sh <checkout> <name>: clean check and build of moq-net, per-crate perf counts.
set -eu
root=$1
name=$2
spike=$(cd "$(dirname "$0")" && pwd)
cargo=/nix/store/85lxjwg78jw6rf7pqkf3vcq6291nrlfh-cargo-1.98.1-x86_64-unknown-linux-gnu/bin/cargo
export RUSTC_WRAPPER=$spike/rustc-perf.sh
cd "$root"
for mode in check build; do
	export CARGO_TARGET_DIR=$spike/target-$name-$mode-clean
	rm -rf "$CARGO_TARGET_DIR"
	rm -f "$spike/$name-$mode-clean.csv"
	PERF_LOG=$spike/$name-$mode-clean.csv "$cargo" "$mode" --locked -p moq-net --lib -q
	rm -rf "$CARGO_TARGET_DIR"
done
