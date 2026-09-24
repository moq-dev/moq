#!/usr/bin/env bash
# Lint or format every tracked shell script: sh/shell.sh check|fix
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

scripts=()
while IFS= read -r -d '' file; do
    if git --literal-pathspecs ls-files --error-unmatch -- "$file" >/dev/null 2>&1; then
        scripts+=("$file")
    fi
done < <(shfmt -f=0 .)
((${#scripts[@]})) || exit 0

case "${1:-}" in
    check)
        shfmt --diff "${scripts[@]}"
        shellcheck "${scripts[@]}"
        ;;
    fix)
        shfmt --write "${scripts[@]}"
        ;;
    *)
        echo "usage: sh/shell.sh check|fix" >&2
        exit 2
        ;;
esac
