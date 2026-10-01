#!/usr/bin/env bash
# Compile what the branch changed since BASE, plus its dependents, on a Windows
# or macOS host. `--all` compiles the whole workspace.
#
# Usage: sh/rs/platform.sh [BASE|--all]
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if [[ "${1:-}" == --all ]]; then
    exec sh/rs/select.sh platform --all
fi

list=$(mktemp)
trap 'rm -f "$list"' EXIT
sh/changed.sh "${1:-}" >"$list"
sh/rs/select.sh platform "$list"
