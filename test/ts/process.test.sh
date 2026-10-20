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

# Invalid grading modes must fail before a build or any network process starts.
mkdir -p "$HARNESS_RUN/bin"
cat >"$HARNESS_RUN/bin/cargo" <<'SH'
#!/usr/bin/env bash
echo "unexpected cargo invocation" >&2
exit 73
SH
chmod +x "$HARNESS_RUN/bin/cargo"
for mode in --rate --live; do
    args=(--loss 1 "$mode")
    [[ "$mode" != --rate ]] || args+=(7000000)
    if PATH="$HARNESS_RUN/bin:$PATH" bash "$DIR/run.sh" "${args[@]}" >"$HARNESS_RUN/${mode#--}.log" 2>&1; then
        echo "accepted random-loss quality grading with $mode" >&2
        exit 1
    fi
    if ! grep -Fq 'error: --loss cannot be combined with --rate or --live' "$HARNESS_RUN/${mode#--}.log"; then
        cat "$HARNESS_RUN/${mode#--}.log" >&2
        echo "did not refuse unsupported random-loss grading with $mode" >&2
        exit 1
    fi
done
status=0
PATH="$HARNESS_RUN/bin:$PATH" bash "$DIR/run.sh" --loss 1 >"$HARNESS_RUN/correctness.log" 2>&1 || status=$?
if [[ "$status" -ne 73 ]]; then
    cat "$HARNESS_RUN/correctness.log" >&2
    echo "standalone random-loss correctness did not reach the guarded build" >&2
    exit 1
fi
echo "PASS grading modes: random loss cannot grade congestion or live timing"
