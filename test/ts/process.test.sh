#!/usr/bin/env bash
set -euo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
# shellcheck source-path=SCRIPTDIR source=process.sh
source "$DIR/process.sh"
harness_begin ts-process "just test ts-controls"

for code in 0 42 124; do
    # shellcheck disable=SC2016 # $1 belongs to the child shell.
    harness_spawn writer "$HARNESS_RUN/writer-$code.log" bash -c 'printf "captured content\n"; exit "$1"' _ "$code"
    if ts_wait "$HARNESS_PID" "writer-$code"; then
        [[ "$code" -eq 0 ]] || {
            echo "accepted failed writer $code" >&2
            exit 1
        }
    else
        [[ "$code" -ne 0 ]] || {
            echo "rejected successful writer" >&2
            exit 1
        }
    fi
    [[ -s "$HARNESS_RUN/writer-$code.log" ]] || {
        echo "control produced no partial output" >&2
        exit 1
    }
    # The grader may stop early, but the writer must finish and the grader's
    # failure must survive draining. No sleeps or wall-clock timeouts are needed.
    set +e
    # shellcheck disable=SC2016 # $1 belongs to the child shell.
    printf 'window\n%1000000s' '' | ts_drain bash -c 'read -r line; exit "$1"' _ "$code"
    statuses=("${PIPESTATUS[@]}")
    set -e
    [[ "${statuses[0]}" -eq 0 && "${statuses[1]}" -eq "$code" ]] || {
        echo "draining changed writer/grader statuses: ${statuses[*]}" >&2
        exit 1
    }
done
echo "PASS process status: partial output cannot hide a failure or timeout"
