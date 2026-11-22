#!/usr/bin/env bash
# Lint or format every tracked shell script: sh/shell.sh check|fix
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Ask shfmt about tracked files only: walking the tree races any tool creating and removing
# untracked directories, such as a concurrent `bun install` under node_modules.
tracked=()
while IFS= read -r -d '' file; do
    # Skip a tracked file deleted from the worktree.
    if [[ -f $file ]]; then
        tracked+=("$file")
    fi
done < <(git ls-files -z)
((${#tracked[@]})) || exit 0

scripts=()
while IFS= read -r -d '' file; do
    scripts+=("$file")
done < <(shfmt -f=0 -- "${tracked[@]}")
# A failed listing would lint a partial set and pass or fail on the wrong files.
wait "$!"
((${#scripts[@]})) || exit 0

case "${1:-}" in
    check)
        shfmt --diff "${scripts[@]}"
        # Without `-x`, shellcheck follows a `source` only to a file in the same invocation,
        # so a script sourcing one from another batch would fail with SC1091.
        cpus=$(getconf _NPROCESSORS_ONLN)
        ((cpus <= 8)) || cpus=8
        printf '%s\0' "${scripts[@]}" | xargs -0 -n 16 -P "$cpus" shellcheck -x --
        ;;
    fix)
        shfmt --write "${scripts[@]}"
        ;;
    *)
        echo "usage: sh/shell.sh check|fix" >&2
        exit 2
        ;;
esac
