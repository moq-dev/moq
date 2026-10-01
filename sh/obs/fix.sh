#!/usr/bin/env bash
# Auto-fix formatting.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../cpp/obs"

if command -v clang-format >/dev/null; then
    git ls-files 'src/*.cpp' 'src/*.h' 'test/*.cpp' | xargs clang-format -i
fi
if command -v gersemi >/dev/null; then
    gersemi --in-place CMakeLists.txt cmake
fi
