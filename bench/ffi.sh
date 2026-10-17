#!/usr/bin/env bash
# Run the binding benchmark: the Rust baselines and every wrapper's runner on this
# machine, then the ratio table. Ratios only hold within one machine, so stale
# results from an earlier run are cleared first.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
export MOQ_BENCH_DIR="${MOQ_BENCH_DIR:-$root/target/bench/ffi}"
if [[ $# == 0 ]]; then
    set -- go py kt cpp dart
    if [[ $(uname -s) == Darwin ]]; then set -- "$@" swift; fi
fi
for runner in "$@"; do
    case "$runner" in
        go | py | kt | cpp | dart) ;;
        swift)
            if [[ $(uname -s) != Darwin ]]; then
                echo "Swift binding benchmark requires macOS" >&2
                exit 1
            fi
            ;;
        *)
            echo "unsupported binding benchmark runner: $runner" >&2
            exit 1
            ;;
    esac
done
rm -rf "$MOQ_BENCH_DIR"

cd "$root"
# Through nextest like every Rust test; each round guards itself against a wedged read.
just rs test -p moq-ffi --cargo-profile release -E 'test(bench::benchmark_ffi)' --run-ignored only
for runner in "$@"; do
    case "$runner" in
        go) just go bench -benchtime="${MOQ_BENCH_SECONDS:-1}s" ;;
        py | kt | cpp | dart | swift) just "$runner" bench ;;
    esac
    case "$runner" in
        py) result=python ;;
        kt) result=kotlin ;;
        *) result=$runner ;;
    esac
    if [[ ! -s "$MOQ_BENCH_DIR/$result.json" ]]; then
        echo "binding benchmark did not produce $result.json" >&2
        exit 1
    fi
done
python3 bench/ffi.py "$MOQ_BENCH_DIR"
