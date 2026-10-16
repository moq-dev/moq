#!/usr/bin/env bash
# Run the binding benchmark: the Rust baselines and every wrapper's runner on this
# machine, then the ratio table. Ratios only hold within one machine, so stale
# results from an earlier run are cleared first.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
export MOQ_BENCH_DIR="${MOQ_BENCH_DIR:-$root/target/bench/ffi}"
rm -rf "$MOQ_BENCH_DIR"

cd "$root"
# Through nextest like every Rust test; each round guards itself against a wedged read.
just rs test -p moq-ffi --cargo-profile release -E 'test(bench::benchmark_ffi)' --run-ignored only
just go bench -benchtime="${MOQ_BENCH_SECONDS:-1}s"
just py bench
just kt bench
python3 bench/ffi.py "$MOQ_BENCH_DIR"
