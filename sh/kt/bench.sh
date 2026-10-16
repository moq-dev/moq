#!/usr/bin/env bash
set -euo pipefail

# Run the binding benchmark's Kotlin runner (see bench/README.md) on a release
# moq-ffi. The runner is a JVM test that skips itself without MOQ_BENCH_DIR, so
# `just kt check` never pays for it.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KT_DIR="$(cd "$SCRIPT_DIR/../../kt" && pwd)"
export MOQ_BENCH_DIR="${MOQ_BENCH_DIR:-$KT_DIR/../target/bench/ffi}"

MOQ_FFI_PROFILE=release bash "$SCRIPT_DIR/generate.sh"
gradle -p "$KT_DIR" -Pmoqffi.version=0.0.0-dev :moq:jvmTest --tests dev.moq.FfiBench --rerun
echo "kt bench: wrote $MOQ_BENCH_DIR/kotlin.json"
