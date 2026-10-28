#!/usr/bin/env bash
# Compile cost of the synthetic crate per style and shape, its llvm-lines, and
# whether a body edit changes its rmeta.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
spike=$(dirname "$here")
cargo=/nix/store/85lxjwg78jw6rf7pqkf3vcq6291nrlfh-cargo-1.98.1-x86_64-unknown-linux-gnu/bin/cargo
export CARGO_TARGET_DIR=$spike/target-synth
export CARGO_INCREMENTAL=0
cd "$here"
out=$spike/synth-results
rm -rf "$out"
mkdir -p "$out"

# Warm the dependencies once.
python3 gen.py hand flat 1
"$cargo" check -q --lib
"$cargo" build -q --lib

for style in hand macro async; do
	for case in "flat 50" "flat 200" "chain 16" "chain 64"; do
		set -- $case
		python3 gen.py "$style" "$1" "$2"
		for mode in check build; do
			for run in 1 2 3; do
				touch src/lib.rs
				PERF_LOG=$out/$style-$1-$2-$mode.csv RUSTC_WRAPPER=$spike/rustc-perf.sh "$cargo" "$mode" -q --lib
			done
		done
	done
done

# llvm-lines for 200 flat machines.
for style in hand macro async; do
	python3 gen.py "$style" flat 200
	CARGO=$cargo cargo-llvm-lines llvm-lines --lib > "$out/$style-llvm-lines.txt" 2>/dev/null
done

# Does editing one body change the crate's metadata?
for style in hand macro async; do
	for tag in "" x; do
		python3 gen.py "$style" flat 10 $tag
		CARGO_TARGET_DIR=$spike/target-rmeta-$style "$cargo" check -q --lib
		sha256sum "$spike"/target-rmeta-$style/debug/deps/libsynth-*.rmeta | cut -c1-16 >> "$out/$style-rmeta.txt"
	done
done

# Expansion's share of a check, for 200 flat machines.
python3 gen.py macro flat 200
touch src/lib.rs
RUSTC_BOOTSTRAP=1 "$cargo" rustc -q --lib -- -Ztime-passes > "$out/macro-time-passes.txt" 2>&1 || true
python3 gen.py hand flat 200
touch src/lib.rs
RUSTC_BOOTSTRAP=1 "$cargo" rustc -q --lib -- -Ztime-passes > "$out/hand-time-passes.txt" 2>&1 || true
