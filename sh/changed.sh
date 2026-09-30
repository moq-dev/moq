#!/usr/bin/env bash
# Print the paths the branch changed since BASE, one per line.
#
# Usage: sh/changed.sh [BASE]
#
# BASE: the argument, then $GITHUB_BASE_REF (a PR checkout has no upstream),
# then the upstream, then origin/main. `git push -u` points the upstream at the
# branch's own remote copy, which would diff HEAD against itself, so that case
# falls through to origin/main.
set -euo pipefail

base=${1:-}

cd "$(git rev-parse --show-toplevel)"

if [[ -z "$base" && -n "${GITHUB_BASE_REF:-}" ]]; then
    base="origin/$GITHUB_BASE_REF"
fi
if [[ -z "$base" ]]; then
    base=$(git rev-parse --abbrev-ref '@{upstream}' 2>/dev/null || true)
    if [[ -z "$base" || "$base" == */"$(git branch --show-current)" ]]; then
        base=origin/main
    fi
fi
merge_base=$(git merge-base "$base" HEAD) || {
    echo "error: cannot resolve merge-base against $base (is full history fetched?)" >&2
    exit 1
}
echo "changed: base $base" >&2

# Untracked files count too: a brand new crate or module is the whole change.
{
    git diff --name-only "$merge_base"
    git ls-files --others --exclude-standard
} | sort -u
