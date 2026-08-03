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
# alert.yml, and pull-request-only workflows must not be (each entry costs a
# skipped Alert run per trigger, and `Check` alone fires on every push to every
# PR).
#
# This is the guard against the list going stale: adding a workflow, or adding
# a push/schedule trigger to a PR-only one, fails here until alert.yml matches.
check_coverage() {
    local expected actual
    # Not `non_pr_workflow_names | sort -u`: a failure inside a pipeline would
    # be swallowed and read as "no workflows need watching", which is the
    # silent pass this whole guard exists to prevent. Capture, propagate, then
    # sort.
    expected=$(non_pr_workflow_names) || return 2
    expected=$(sort -u <<<"$expected")
    actual=$(watched_workflow_names) || return 2
    actual=$(sort -u <<<"$actual")

    if [[ "$expected" == "$actual" ]]; then
        echo "alert.yml watches all $(wc -l <<<"$expected" | tr -d ' ') non-PR workflows"
        return 0
    fi

    echo "alert.yml's workflow list is out of sync with .github/workflows/" >&2
    comm -23 <(echo "$expected") <(echo "$actual") | sed 's/^/  missing (add to alert.yml):  /' >&2
    comm -13 <(echo "$expected") <(echo "$actual") | sed 's/^/  stale (remove, PR-only):    /' >&2
    return 1
}

# Names of workflows carrying at least one non-pull_request trigger.
#
# The YAML is parsed rather than pattern-matched. GitHub accepts `on:` as a
# block map, block sequence, flow sequence or bare scalar, any of which may wrap
# across lines, and it runs both .yml and .yaml. A matcher covering only some of
# those shapes reports "no non-PR triggers" for a workflow it did not actually
# read, which is the silent gap this guard exists to close. bun is already a
# hard dependency (jsDeps in flake.nix, `bun install` in CI), so its YAML parser
# costs nothing new.
#
# A workflow with no `name:` is an error, not a skip: GitHub falls back to the
# file path, which alert.yml cannot reference, and skipping it would put the
# workflow past this check unwatched.
non_pr_workflow_names() {
    local f files=()
    shopt -s nullglob
    for f in "$WORKFLOWS_DIR"/*.yml "$WORKFLOWS_DIR"/*.yaml; do
        # alert.yml watches the others; watching itself would be a trigger loop.
        [[ "$(basename "$f")" =~ ^alert\.ya?ml$ ]] && continue
        files+=("$f")
    done

    printf "%s\n" "${files[@]}" | bun -e '
const files = (await Bun.stdin.text()).split("\n").filter(Boolean);
// Both report their failure as a check on the PR itself, so alert.yml skips
// them at runtime and a workflow triggered only by these needs no entry.
const PR_EVENTS = new Set(["pull_request", "pull_request_target"]);
const names = [];
for (const file of files) {
    const doc = Bun.YAML.parse(await Bun.file(file).text());
    if (doc === null || typeof doc !== "object" || Array.isArray(doc)) {
        console.error("alert.sh: " + file + " is not a YAML mapping");
        process.exit(2);
    }
    const on = doc.on;
    let triggers;
    if (typeof on === "string") triggers = [on];
    else if (Array.isArray(on)) triggers = on;
    else if (on !== null && typeof on === "object") triggers = Object.keys(on);
    else {
        console.error("alert.sh: cannot read the on: value of " + file);
        process.exit(2);
    }
    if (!triggers.some((t) => !PR_EVENTS.has(t))) continue;

    const name = doc.name;
    if (typeof name !== "string" || name.trim() === "") {
        console.error("alert.sh: " + file + " has no top-level name:, so alert.yml cannot reference it");
        process.exit(2);
    }
    names.push(name.trim());
}
if (names.length) console.log(names.join("\n"));
'
}

# Names listed under alert.yml's `on.workflow_run.workflows:`.
watched_workflow_names() {
    ALERT_YML="$WORKFLOWS_DIR/alert.yml" bun -e '
const doc = Bun.YAML.parse(await Bun.file(Bun.env.ALERT_YML).text());
const list = doc?.on?.workflow_run?.workflows;
if (!Array.isArray(list)) {
    console.error("alert.sh: alert.yml has no on.workflow_run.workflows list");
    process.exit(2);
}
if (list.length) console.log(list.join("\n"));
'
}

case "${1:-}" in
    discord) discord ;;
    check-coverage) check_coverage ;;
    *)
        echo "Usage: $0 {discord|check-coverage}" >&2
        exit 1
        ;;
esac
