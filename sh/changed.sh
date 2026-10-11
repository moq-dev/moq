#!/usr/bin/env bash
# Print the paths the branch changed since BASE, one per line.
#
# Usage: sh/changed.sh [BASE]
#
# BASE: the argument, then $GITHUB_BASE_REF, then the open PR's base,
# then an upstream named main or release, then origin/main.
# For an unpublished or offline stack, pass its base explicitly: an arbitrary
# upstream cannot distinguish a stacked base from a differently named PR head.
set -euo pipefail

base=${1:-}

cd "$(git rev-parse --show-toplevel)"

if [[ -z "$base" && -n "${GITHUB_BASE_REF:-}" ]]; then
    base="origin/$GITHUB_BASE_REF"
fi
if [[ -z "$base" ]]; then
    upstream=$(git rev-parse --abbrev-ref '@{upstream}' 2>/dev/null || true)
    upstream_head=${upstream#*/}
    head=$(git branch --show-current)
    heads=("$head")
    case "$upstream_head" in
        main | release | "") ;;
        *) [[ "$upstream_head" == "$head" ]] || heads+=("$upstream_head") ;;
    esac

    # Prefer the local branch's PR; an upstream may be its stacked base.
    # Tracking a PR head under a different local name must also find that PR.
    # Bound optional network lookups and disable authentication prompts.
    if command -v gh >/dev/null && command -v timeout >/dev/null; then
        for head in "${heads[@]}"; do
            [[ -n "$head" ]] || continue
            pr_base=$(GH_PROMPT_DISABLED=1 timeout 3s gh pr view "$head" \
                --json baseRefName,state --jq 'select(.state == "OPEN") | .baseRefName' 2>/dev/null || true)
            if [[ -n "$pr_base" ]]; then
                if [[ "$upstream_head" == "$pr_base" ]]; then
                    base=$upstream
                else
                    base="origin/$pr_base"
                fi
                break
            fi
        done
    fi
    if [[ -z "$base" ]]; then
        case "$upstream_head" in
            main | release) base=$upstream ;;
            *) base=origin/main ;;
        esac
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
