#!/usr/bin/env bash
# Print the full-suite command a merge group's PR needs in place of the checks
# its diff selects, or nothing.
# Usage: queue-suite.sh <merge group head ref> [base, default origin/main]
set -euo pipefail

if [[ $# == 0 || $# -gt 2 ]]; then
    echo "usage: queue-suite.sh <merge group head ref> [base]" >&2
    exit 2
fi
pr=$(sed -nE 's#^(refs/heads/)?gh-readonly-queue/[^/]+/pr-([0-9]+)-[0-9a-f]+$#\2#p' <<<"$1")
if [[ -z $pr ]]; then
    echo "queue-suite: no PR number in the merge group ref $1" >&2
    exit 1
fi
repo=${GITHUB_REPOSITORY:?}
base=${2:-origin/main}

# A high-risk head, or one no review record covers, needs the base's RISK.md suite.
suite=$(quest suite "${GITHUB_SERVER_URL:-https://github.com}/$repo/pull/$pr" --base "$base")
if [[ -z $suite ]]; then
    # The record is in the PR body, which its author writes, so an author
    # without write access gets the full suite whatever the record says.
    author=$(gh api "repos/$repo/pulls/$pr" --jq .user.login)
    permission=$(gh api "repos/$repo/collaborators/$author/permission" --jq .permission)
    case $permission in
        admin | write) ;;
        *)
            # `quest suite` just fetched the base; its RISK.md holds one fenced command.
            suite=$(git show "$base:RISK.md" | awk '
            /^## / { inside = ($0 == "## Full suite"); next }
            inside && /^```/ { if (fenced) exit; fenced = 1; next }
            inside && fenced { print }')
            ;;
    esac
fi
if [[ -n $suite ]]; then
    echo "$suite"
fi
