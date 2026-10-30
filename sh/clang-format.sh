#!/usr/bin/env bash
# Format the handwritten C and C++ under each DIR, relative to the repository
# root, with the OBS plugin's style (cpp/obs/.clang-format), or with --check
# fail on any file it would change. The generated bindings are gitignored, so
# `git ls-files` never lists them.
set -euo pipefail

usage="usage: $0 [--check] DIR..."
args=(-i)
if [[ "${1:-}" == --check ]]; then
    args=(--dry-run --Werror)
    shift
fi
if (($# == 0)); then
    echo "$usage" >&2
    exit 2
fi

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Windows and macOS hosts outside the dev shell may have none; there CI_STRICT
# fails the module up front instead (see sh/dispatch.sh).
if ! command -v clang-format >/dev/null 2>&1; then
    echo "clang-format: not on PATH, skipping" >&2
    exit 0
fi

patterns=()
for dir in "$@"; do
    for extension in c h cc cpp hpp; do
        patterns+=("$dir/*.$extension")
    done
done
git ls-files -z --cached --others --exclude-standard -- "${patterns[@]}" |
    xargs -0 clang-format --style=file:cpp/obs/.clang-format "${args[@]}"
