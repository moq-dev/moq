#!/usr/bin/env bash
# Check every tracked text file, including files unchanged by the branch.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Git decides what is binary. Lockfiles are generated dependency metadata.
# Spell the UTF-8 bytes as escapes so the match works in every locale.
status=0
git --no-pager grep --no-textconv -n -I -F $'\xe2\x80\x94' -- . ':!*.lock' || status=$?
case "$status" in
    0)
        echo "error: em dashes are forbidden; replace them with plain punctuation (UI placeholders use '-')." >&2
        exit 1
        ;;
    1) exit 0 ;;
    *) exit "$status" ;;
esac
