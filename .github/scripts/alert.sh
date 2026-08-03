#!/usr/bin/env bash
#
# Discord alerting for workflow failures outside of pull requests.
# Usage:
#   alert.sh discord          Post the current workflow_run failure to Discord.
#   alert.sh check-coverage   Verify alert.yml watches every non-PR workflow.
#
# Environment (discord):
#   DISCORD_WEBHOOK   Required. The #alerts channel webhook URL.
#   RUN_NAME          Workflow name (github.event.workflow_run.name).
#   RUN_URL           Run html_url.
#   RUN_CONCLUSION    failure / timed_out.
#   RUN_EVENT         What triggered the failed run (push, schedule, ...).
#   RUN_REF           head_branch: a branch name, or a tag for tag-triggered runs.
#   RUN_TITLE         display_title, usually the commit subject.
#   RUN_ACTOR         Triggering actor login.

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

# Print one trigger name per line for a workflow file.
#
# GitHub accepts four serializations of `on:` (block map, block sequence, flow
# sequence, bare scalar), so all four are handled here. A parser that understood
# only one would report "no non-PR triggers" for a workflow it merely failed to
# read, and check_coverage would pass while the workflow went unwatched. That
# silent gap is the whole thing this script exists to prevent.
#
# Anything still unrecognized (a quoted `"on":`, say) is loud rather than empty:
# it prints to stderr and yields no triggers, which check_coverage then reads as
# non-PR, so the workflow is reported missing instead of quietly skipped.
workflow_triggers() {
    awk -v file="$1" '
        # on: [push, pull_request]
        /^on:[[:space:]]*\[/ {
            line = $0
            sub(/^on:[[:space:]]*\[/, "", line)
            sub(/\].*$/, "", line)
            n = split(line, items, ",")
            for (i = 1; i <= n; i++) {
                gsub(/[[:space:]"'"'"']/, "", items[i])
                if (items[i] != "") print items[i]
            }
            seen = 1
            next
        }
        # on: push
        /^on:[[:space:]]*[A-Za-z_]/ {
            line = $0
            sub(/^on:[[:space:]]*/, "", line)
            sub(/[[:space:]]*#.*$/, "", line)
            print line
            seen = 1
            next
        }
        # on:  (block map or block sequence on the following indented lines)
        /^on:[[:space:]]*(#.*)?$/ { in_on = 1; seen = 1; next }

        in_on && /^[^[:space:]#]/ { in_on = 0 }
        !in_on { next }
        # Blank lines and comments.
        /^[[:space:]]*(#.*)?$/ { next }
        # Block sequence item: "  - push"
        /^  - [A-Za-z_]/ {
            line = $0
            sub(/^  - /, "", line)
            sub(/[[:space:]]*#.*$/, "", line)
            print line
            next
        }
        # Block map key: "  push:"
        /^  [A-Za-z_][A-Za-z_0-9]*:/ {
            line = $1
            sub(/:.*/, "", line)
            print line
            next
        }
        # Deeper indentation is a triggers own config (branches, tags, types).
        /^    / { next }
        {
            printf "alert.sh: cannot parse the on: block of %s (line %d: %s)\n", file, NR, $0 >"/dev/stderr"
            exit 2
        }
        END {
            if (!seen) {
                printf "alert.sh: no on: block found in %s\n", file >"/dev/stderr"
                exit 2
            }
        }
    ' "$1"
}

# Names of workflows carrying at least one non-pull_request trigger.
#
# GitHub runs both .yml and .yaml, so both are scanned. nullglob keeps the
# unmatched extension from expanding to a literal path under `set -u`.
non_pr_workflow_names() {
    local f triggers
    shopt -s nullglob
    for f in "$WORKFLOWS_DIR"/*.yml "$WORKFLOWS_DIR"/*.yaml; do
        # alert.yml watches the others; watching itself would be a trigger loop.
        [[ "$(basename "$f")" =~ ^alert\.ya?ml$ ]] && continue

        triggers=$(workflow_triggers "$f")
        if grep -qvx 'pull_request' <<<"$triggers"; then
            sed -n 's/^name:[[:space:]]*//p' "$f" | head -1
        fi
    done
}

# Names listed under alert.yml's `on.workflow_run.workflows:`.
watched_workflow_names() {
    awk '
        /^    workflows:/ { in_list = 1; next }
        in_list && /^      - / {
            sub(/^      - /, "")
            print
            next
        }
        in_list { exit }
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
