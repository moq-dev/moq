#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
if [[ $(uname -s) != Darwin ]]; then
    echo "Swift binding benchmark requires macOS" >&2
    exit 1
fi
for tool in swift cargo xcodebuild; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Swift binding benchmark requires $tool" >&2
        exit 1
    fi
done
export MOQ_BENCH_DIR="${MOQ_BENCH_DIR:-$root/target/bench/ffi}"
export MOQ_FFI_PROFILE=release
bash "$root/sh/swift/check.sh" --configuration release --filter FfiBench/testBenchmark
