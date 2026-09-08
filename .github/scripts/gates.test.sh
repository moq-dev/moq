#!/usr/bin/env bash
#
# Fixtures for the aggregate verdict. The cases that matter are the ones that
# look green: a lane the diff selected that never ran reports `skipped`, exactly
# like a lane the diff did not need.

set -euo pipefail

scripts="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() {
    echo "gates: $1" >&2
    exit 1
}

# A `toJSON(needs)` payload: the selector's map plus one result per job.
needs() {
    local map=$1 results=$2
    printf '{"select":{"result":"success","outputs":%s},%s}' "$map" "$results"
}

passes() {
    GATES_NEEDS="$1" "$scripts/gates.sh" >/dev/null 2>&1 || fail "$2"
}

fails() {
    ! GATES_NEEDS="$1" "$scripts/gates.sh" >/dev/null 2>&1 || fail "$2"
}

map='{"smoke":"true","wasm":"false"}'

passes "$(needs "$map" '"smoke":{"result":"success"},"wasm":{"result":"skipped"}')" \
    "a selected lane that passed and an irrelevant one must aggregate green"

# The whole reason this is a script. Both lanes report `skipped`; only the
# selector knows that one of them was needed.
fails "$(needs "$map" '"smoke":{"result":"skipped"},"wasm":{"result":"skipped"}')" \
    "a selected lane that never ran must not pass as irrelevant"

fails "$(needs "$map" '"smoke":{"result":"failure"},"wasm":{"result":"skipped"}')" \
    "a failed lane must fail"

# A timed-out job reports `failure`; a superseded one reports `cancelled`. Both
# mean the lane did not prove anything.
fails "$(needs "$map" '"smoke":{"result":"cancelled"},"wasm":{"result":"skipped"}')" \
    "a cancelled lane must fail"

# The wiring checks. A lane with no job behind it never runs and never reports,
# and a job with no lane in front of it is never selected and never runs.
fails "$(needs '{"smoke":"true","ts":"true"}' '"smoke":{"result":"success"}')" \
    "a lane with no job must fail"
fails "$(needs '{"smoke":"true"}' '"smoke":{"result":"success"},"ts":{"result":"success"}')" \
    "a job with no lane must fail"

# An unselected lane that ran anyway means the job's `if` and the impact map
# disagree, which is the same bug seen from the other side.
fails "$(needs "$map" '"smoke":{"result":"success"},"wasm":{"result":"success"}')" \
    "an unselected lane that ran must fail"

# Docs-only: nothing selected, nothing ran, and the required check still reports.
passes "$(needs '{"smoke":"false","wasm":"false"}' '"smoke":{"result":"skipped"},"wasm":{"result":"skipped"}')" \
    "a docs-only pull request must aggregate green"

# Without the selector every lane would be compared against an empty map and
# pass by being skipped.
fails '{"select":{"result":"failure","outputs":{}},"smoke":{"result":"skipped"}}' \
    "a failed selector must fail the aggregate"

fails '{"select":{"result":"success","outputs":{}}}' \
    "an empty impact map must fail rather than pass vacuously"

# A merged pull request's closed event has the base branch ref, so the pull
# request number is the stable identity that cancels its still-running jobs.
grep -qF 'group: gates-${{ github.event.pull_request.number }}' "$scripts/../workflows/gates.yml" ||
    fail "the concurrency group must stay stable across pull request events"

echo "gates: aggregate ok"
