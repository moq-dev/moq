#!/usr/bin/env bash
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/../.." && pwd)
export MOQ_FFI_PROFILE=release
staged=$(bash "$here/stage.sh" --output "$root/dist/runtime-bench")
wrapper=$(printf '%s\n' "$staged" | sed -n 2p)
cd "$wrapper"
CGO_ENABLED=1 GOFLAGS=-mod=mod go test -run '^$' -bench '^BenchmarkFFI$' "$@"
