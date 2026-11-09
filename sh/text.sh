#!/usr/bin/env bash
# Check every tracked text file, including files unchanged by the branch.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

failed=0

# Fail with the message when git grep matches. Matches stay on stdout. Stderr
# is captured because Git reports a file it cannot read there, yet still exits
# 1 when nothing else matches.
forbid() {
    local message=$1 errors status=0
    shift
    { errors="$(git --no-pager grep --no-textconv -n -I "$@" 2>&1 >&3 3>&-)" || status=$?; } 3>&1
    if [[ -n "$errors" ]]; then
        printf '%s\n' "$errors" >&2
        echo "error: git grep could not scan every tracked file." >&2
        exit $((status > 1 ? status : 2))
    fi
    case "$status" in
        0)
            echo "error: $message" >&2
            failed=1
            ;;
        1) ;;
        *) exit "$status" ;;
    esac
}

# Git decides what is binary. Lockfiles are generated dependency metadata.
# Spell the UTF-8 bytes as escapes so the match works in every locale.
forbid "em dashes are forbidden; replace them with plain punctuation (UI placeholders use '-')." \
    -F $'\xe2\x80\x94' -- . ':!*.lock'

# A public Rust item is documented or not public.
forbid "#[doc(hidden)] is forbidden; make the item private, delete it, or document it." \
    -E 'doc[[:space:]]*\([[:space:]]*hidden' -- '*.rs'

exit "$failed"
