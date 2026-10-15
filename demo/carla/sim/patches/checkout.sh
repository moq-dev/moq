#!/usr/bin/env bash
# A shallow checkout of exactly one commit, for build.sh. A checkout already at
# that commit is left alone so a re-run keeps its build outputs. The fetch lands
# in a staging directory renamed into place only once it succeeds, so a failed
# fetch (no Epic authorization, a dropped connection) leaves nothing that blocks
# the next attempt.
set -euo pipefail

dir="${1:?usage: checkout.sh <dir> <url> <rev> [git-config-args...]}"
url="${2:?usage: checkout.sh <dir> <url> <rev> [git-config-args...]}"
rev="${3:?usage: checkout.sh <dir> <url> <rev> [git-config-args...]}"
shift 3

if [[ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" == "$rev" ]]; then
    exit 0
fi
if [[ -e "$dir" ]]; then
    echo "checkout: $dir exists but is not at $rev; move it aside" >&2
    exit 1
fi
staging="$dir.partial"
rm -rf "$staging"
git init -q "$staging"
git -C "$staging" "$@" fetch --depth 1 "$url" "$rev"
git -C "$staging" checkout -q --detach FETCH_HEAD
mv "$staging" "$dir"
