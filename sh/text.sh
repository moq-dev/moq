#!/usr/bin/env bash
# Check every tracked text file, including files unchanged by the branch.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Git decides what is binary. Lockfiles are generated dependency metadata.
# Spell the UTF-8 bytes as escapes so the match works in every locale.
# Matches stay on stdout. Stderr is captured because Git reports a file it
# cannot read there, yet still exits 1 when nothing else matches.
status=0
{ errors="$(git --no-pager grep --no-textconv -n -I -F $'\xe2\x80\x94' -- . ':!*.lock' 2>&1 >&3 3>&-)" || status=$?; } 3>&1
if [[ -n "$errors" ]]; then
    printf '%s\n' "$errors" >&2
    echo "error: git grep could not scan every tracked file." >&2
    exit $((status > 1 ? status : 2))
fi
case "$status" in
    0)
        echo "error: em dashes are forbidden; replace them with plain punctuation (UI placeholders use '-')." >&2
        exit 1
        ;;
    1) exit 0 ;;
    *) exit "$status" ;;
esac
