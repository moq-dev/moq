#!/usr/bin/env bash
# Load the latest completed history, then retain the new report even if growth fails.
set -euo pipefail
out=${1:?usage: nightly.sh OUTPUT RUN}
run=${2:?usage: nightly.sh OUTPUT RUN}
mkdir -p "$out"
history=()
# A packaging or collector failure may leave no history artifact in the newest run.
# Search retained completed runs rather than silently discarding the older baseline.
runs=$(gh run list --workflow nightly.yml --branch main --limit 45 --status completed --json databaseId --jq '.[].databaseId')
while IFS= read -r prior; do
    [[ -n "$prior" ]] || continue
    artifact=$(gh api "repos/{owner}/{repo}/actions/runs/$prior/artifacts" --jq '.artifacts[] | select(.name == "nightly-size-history" and .expired == false) | .id')
    if [[ -n "$artifact" ]]; then
        previous="$out/previous/$prior"
        gh run download "$prior" --name nightly-size-history --dir "$previous"
        # Failed collectors can upload diagnostics without producing a history.
        if [[ -f "$previous/history.json" ]]; then
            history=(--history "$previous/history.json")
            break
        fi
    fi
done <<<"$runs"
sh/size/collect.sh "$out" "$run"
status=0
python3 sh/size/report.py "$out" "${history[@]}" || status=$?
{
    cat "$out/summary.md"
    printf '\n<details><summary>FFI crate attribution</summary>\n\n```text\n'
    cat "$out/ffi-bloat.txt"
    printf '\n```\n</details>\n'
} >>"${GITHUB_STEP_SUMMARY:-/dev/stdout}"
exit "$status"
