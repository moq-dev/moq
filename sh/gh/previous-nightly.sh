#!/usr/bin/env bash
# Print the commit of the latest scheduled Nightly on main other than this run.
set -euo pipefail

sha=$(gh run list --workflow nightly.yml --branch main --event schedule --limit 2 \
    --json databaseId,headSha --jq "map(select(.databaseId != ${GITHUB_RUN_ID:-0}))[0].headSha // empty")
if [[ -z $sha ]]; then
    echo "nightly: no previous scheduled Nightly on main" >&2
    exit 1
fi
echo "$sha"
