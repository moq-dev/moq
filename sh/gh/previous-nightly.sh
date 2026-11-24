#!/usr/bin/env bash
# Print the commit of the latest scheduled Nightly on main other than this run;
# with --run, its run ID and then the commit.
set -euo pipefail

fields='.headSha'
if [[ ${1:-} == --run ]]; then
    fields='"\(.databaseId) \(.headSha)"'
elif [[ $# != 0 ]]; then
    echo "usage: previous-nightly.sh [--run]" >&2
    exit 2
fi
line=$(gh run list --workflow nightly.yml --branch main --event schedule --limit 2 \
    --json databaseId,headSha --jq "map(select(.databaseId != ${GITHUB_RUN_ID:-0}))[0] // empty | $fields")
if [[ -z $line ]]; then
    echo "nightly: no previous scheduled Nightly on main" >&2
    exit 1
fi
echo "$line"
