#!/usr/bin/env bash
# Reject a binary file the branch adds or grows past 500 KiB. Git keeps every
# version forever, so each one bloats every clone. Git decides what is binary,
# so a new format needs no list. sh/large-files.allow names the exceptions.
#
# Usage: sh/large-files.sh [BASE|--all]
#
# BASE resolves as in sh/changed.sh. --all checks every file instead.
set -euo pipefail

cap=$((500 * 1024))
allowlist=sh/large-files.allow

cd "$(git rev-parse --show-toplevel)"

if [[ "${1:-}" == --all ]]; then
    base=$(git hash-object -t tree /dev/null)
else
    base=$(sh/changed.sh --merge-base "${1:-}")
fi

status=0
declare -A allowed=()
while read -r path reason; do
    [[ -n "$path" && "$path" != \#* ]] || continue
    allowed[$path]=1
    if [[ -z "$reason" ]]; then
        echo "error: $allowlist: $path needs a reason" >&2
        status=1
    elif [[ ! -f "$path" ]] || (($(wc -c <"$path") <= cap)); then
        echo "error: $allowlist: $path is not a file over the cap, so remove it" >&2
        status=1
    fi
done <"$allowlist"

index=$(mktemp)
numstat=$(mktemp)
trap 'rm -f "$index" "$numstat"' EXIT

# Untracked files count too, as in sh/changed.sh. An intent-to-add entry in a
# scratch index puts them in the diff without touching the real index.
cp "$(git rev-parse --git-path index)" "$index"
GIT_INDEX_FILE=$index git add --intent-to-add .
GIT_INDEX_FILE=$index git diff --numstat --no-renames -z "$base" >"$numstat"

# A binary file reports - for its line counts.
large=()
while IFS=$'\t' read -r -d '' added _ path; do
    [[ "$added" == - && -f "$path" && ! -L "$path" && -z "${allowed[$path]:-}" ]] || continue
    size=$(wc -c <"$path")
    ((size > cap)) || continue
    if old=$(git cat-file -s "$base:$path" 2>/dev/null) && ((size <= old)); then
        continue
    fi
    large+=("$path ($((size)) bytes)")
done <"$numstat"

if ((${#large[@]})); then
    echo "error: these binary files are over the $cap-byte (500 KiB) cap:" >&2
    printf '       %s\n' "${large[@]}" >&2
    echo "       recompress them, or list each in $allowlist with a reason" >&2
    status=1
fi

exit "$status"
