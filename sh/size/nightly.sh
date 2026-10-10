#!/usr/bin/env bash
# Load the latest completed history, then retain the new report even if growth fails.
set -euo pipefail
out=${1:?usage: nightly.sh OUTPUT PACKAGED_ARTIFACTS}
packaged=${2:?usage: nightly.sh OUTPUT PACKAGED_ARTIFACTS}
mkdir -p "$out"
history=()
# A packaging or collector failure may leave no history artifact in the newest run.
# Search retained completed runs rather than silently discarding the older baseline.
runs=$(gh run list --workflow nightly.yml --branch main --limit 45 --status completed --json databaseId --jq '.[].databaseId')
while IFS= read -r run; do
    [[ -n "$run" ]] || continue
    artifact=$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run/artifacts" --jq '.artifacts[] | select(.name == "nightly-size-history" and .expired == false) | .id')
    if [[ -n "$artifact" ]]; then
        gh run download "$run" --name nightly-size-history --dir "$out/previous"
        history=(--history "$out/previous/history.json")
        break
    fi
done <<< "$runs"
sh/size/collect.sh "$out" "$packaged"
status=0
python3 sh/size/report.py "$out" "${history[@]}" || status=$?
cat "$out/summary.md" >> "$GITHUB_STEP_SUMMARY"
exit "$status"
