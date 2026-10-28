#!/usr/bin/env bash
# measure.sh <checkout> <name>: clean and edit builds of moq-net, per-crate perf counts.
set -eu
root=$1
name=$2
spike=$(cd "$(dirname "$0")" && pwd)
cargo=/nix/store/85lxjwg78jw6rf7pqkf3vcq6291nrlfh-cargo-1.98.1-x86_64-unknown-linux-gnu/bin/cargo
export RUSTC_WRAPPER=$spike/rustc-perf.sh
file=$root/rs/moq-net/src/lite/subscriber.rs
cp "$file" "$spike/$name.orig.rs"
cd "$root"
trap 'cp "$spike/$name.orig.rs" "$file"' EXIT

for mode in check build; do
	export CARGO_TARGET_DIR=$spike/target-$name-$mode
	rm -rf "$CARGO_TARGET_DIR"
	PERF_LOG=$spike/$name-$mode-clean.csv "$cargo" "$mode" --locked -p moq-net --lib -q
	for where in in above; do
		for n in 1 2 3 4; do
			python3 "$spike/edit.py" "$file" "$where" "$n"
			PERF_LOG=$spike/$name-$mode-$where-$n.csv "$cargo" "$mode" --locked -p moq-net --lib -q
			cp "$spike/$name.orig.rs" "$file"
		done
	done
done
