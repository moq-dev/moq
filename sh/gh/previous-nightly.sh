#!/usr/bin/env bash
# Print the commit of the latest scheduled Nightly on main other than this run.
set -euo pipefail

if [[ $# != 0 ]]; then
    echo "usage: previous-nightly.sh" >&2
    exit 2
fi
commit=$(gh run list --workflow nightly.yml --branch main --event schedule --limit 2 \
    --json databaseId,headSha --jq "map(select(.databaseId != ${GITHUB_RUN_ID:-0}))[0].headSha // empty")
if [[ -z $commit ]]; then
    echo "nightly: no previous scheduled Nightly on main" >&2
    exit 1
fi
echo "$commit"
