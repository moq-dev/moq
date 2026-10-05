#!/usr/bin/env bash
# Detect the CMake preset for the current platform (or use the override).
# Pass `ci` as the second argument for the warnings-as-errors variant, whose
# name upstream spells differently on each platform.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../../cpp/obs"
override=${1:-}
variant=${2:-}

if [[ -n "$override" ]]; then
    echo "$override"
elif [[ "$OSTYPE" == "darwin"* ]]; then
    [[ "$variant" == "ci" ]] && echo "macos-ci" || echo "macos"
elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
    [[ "$variant" == "ci" ]] && echo "ubuntu-ci-x86_64" || echo "ubuntu-x86_64"
elif [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" ]]; then
    [[ "$variant" == "ci" ]] && echo "windows-ci-x64" || echo "windows-x64"
else
    echo "Unknown platform: $OSTYPE" >&2
    exit 1
fi
