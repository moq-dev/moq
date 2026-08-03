#!/usr/bin/env bash
#
# Discord alerting for workflow failures outside of pull requests.
# Usage:
#   alert.sh discord           — post the current workflow_run failure to Discord
#   alert.sh check-coverage    — verify alert.yml watches every non-PR workflow
#
# Environment (discord):
#   DISCORD_WEBHOOK   — required, the #alerts channel webhook URL
#   RUN_NAME          — workflow name (github.event.workflow_run.name)
#   RUN_URL           — run html_url
#   RUN_CONCLUSION    — failure / timed_out
#   RUN_EVENT         — what triggered the failed run (push, schedule, ...)
#   RUN_REF           — head_branch: a branch name, or a tag for tag-triggered runs
#   RUN_TITLE         — display_title (usually the commit subject)
#   RUN_ACTOR         — triggering actor login

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKFLOWS_DIR="$(cd "$SCRIPT_DIR/../workflows" && pwd)"

# Post a red embed naming the workflow, what it was reacting to, and a link.
# jq builds the body so a commit subject with quotes or newlines can't produce
# malformed JSON.
discord() {
    : "${DISCORD_WEBHOOK:?DISCORD_WEBHOOK is required}"

    local payload
    payload=$(jq -n \
        --arg title "${RUN_NAME:-workflow} ${RUN_CONCLUSION:-failed}" \
        --arg url "${RUN_URL:-}" \
        --arg desc "${RUN_TITLE:-}" \
        --arg ref "${RUN_REF:-unknown}" \
        --arg event "${RUN_EVENT:-unknown}" \
        --arg actor "${RUN_ACTOR:-unknown}" \
        '{
            embeds: [{
                title: $title,
                url: $url,
                description: $desc,
                color: 15548997,
                fields: [
                    { name: "Ref", value: $ref, inline: true },
                    { name: "Trigger", value: $event, inline: true },
                    { name: "Actor", value: $actor, inline: true }
                ]
            }]
        }')

    # Fail the step on a non-2xx so a rotated/rejected webhook surfaces as a
    # red run rather than silently dropping every future alert.
    curl -sS --fail-with-body -X POST \
        -H "Content-Type: application/json" \
        -d "$payload" \
        "$DISCORD_WEBHOOK"
}

# Every workflow that can run outside a pull request must be watched by
# alert.yml, and PR-only workflows must not be (each entry costs a skipped
# Alert run per trigger, and `Check` alone fires on every push to every PR).
#
# This is the guard against the list going stale: adding a workflow, or adding
# a push/schedule trigger to a PR-only one, fails here until alert.yml matches.
check_coverage() {
    local expected actual
    expected=$(non_pr_workflow_names | sort -u)
    actual=$(watched_workflow_names | sort -u)

    if [[ "$expected" == "$actual" ]]; then
        echo "alert.yml watches all $(wc -l <<<"$expected" | tr -d ' ') non-PR workflows"
        return 0
    fi

    echo "alert.yml's workflow list is out of sync with .github/workflows/" >&2
    comm -23 <(echo "$expected") <(echo "$actual") | sed 's/^/  missing (add to alert.yml):  /' >&2
    comm -13 <(echo "$expected") <(echo "$actual") | sed 's/^/  stale (remove, PR-only):    /' >&2
    return 1
}

# Names of workflows with at least one non-pull_request trigger. Reads the
# top-level `on:` block, which ends at the next column-0 key.
non_pr_workflow_names() {
    local f
    for f in "$WORKFLOWS_DIR"/*.yml; do
        # alert.yml watches others; watching itself would be a trigger loop.
        [[ "$(basename "$f")" == "alert.yml" ]] && continue

        awk '
            /^on:/                  { in_on = 1; next }
            in_on && /^[a-zA-Z_]+:/ { in_on = 0 }
            in_on && /^  [a-z_]+:/  {
                key = $1
                sub(":", "", key)
                if (key != "pull_request") non_pr = 1
            }
            END { exit !non_pr }
        ' "$f" || continue

        sed -n 's/^name:[[:space:]]*//p' "$f" | head -1
    done
}

# Names listed under alert.yml's `on.workflow_run.workflows:`.
watched_workflow_names() {
    awk '
        /^    workflows:/   { in_list = 1; next }
        in_list && /^      - / { sub(/^      - /, ""); print; next }
        in_list             { exit }
    ' "$WORKFLOWS_DIR/alert.yml"
}

case "${1:-}" in
    discord) discord ;;
    check-coverage) check_coverage ;;
    *)
        echo "Usage: $0 {discord|check-coverage}" >&2
        exit 1
        ;;
esac
