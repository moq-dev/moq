#!/usr/bin/env bash
# Benchmark sessions on one host measure under each other's load, so each holds
# the host's benchmark lock and a second one waits, naming the holder.
#
#     source bench/lock.sh; bench_lock "$0" "$@"   # for the rest of this shell
#     bench/lock.sh COMMAND [ARGS...]               # for COMMAND
#
# flock belongs to the open file, which every process the session starts
# inherits, so the lock lasts until the session and everything it left running
# exit, and never outlives them.

# Holds the lock for the rest of the calling shell, recording ARGS as its command.
bench_lock() {
    # A nested session runs inside its caller's.
    [[ -z ${BENCH_LOCK_HELD:-} ]] || return 0
    # A fixed path rather than $TMPDIR, so every checkout, agent, and shell on the host shares it.
    local path=/tmp/moq-bench.lock
    exec 9<>"$path" || return
    python3 "$(dirname "${BASH_SOURCE[0]}")/lock.py" "$path" 9 "$$" "$@" || return
    export BENCH_LOCK_HELD=1
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    set -euo pipefail
    if [[ $# == 0 ]]; then
        echo 'usage: bench/lock.sh COMMAND [ARGS...]' >&2
        exit 2
    fi
    bench_lock "$@"
    exec "$@"
fi
