#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
export MOQ_BENCH_DIR="${MOQ_BENCH_DIR:-$root/target/bench/ffi}"
build="$root/cpp/moq/build/bench"
cmake -S "$root/cpp/moq" -B "$build/package" -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_LIBDIR=lib
cmake --build "$build/package" --config Release
cmake --install "$build/package" --config Release --prefix "$build/prefix"
cmake -S "$root/cpp/moq/bench" -B "$build/runner" -DCMAKE_BUILD_TYPE=Release -DCMAKE_PREFIX_PATH="$build/prefix"
cmake --build "$build/runner" --config Release
"$build/runner/ffi-bench"
