#!/usr/bin/env bash
# Admit a local suite once; cheap selection runs before calling this launcher.
set -euo pipefail

if [[ $# == 0 ]]; then
    echo 'usage: sh/local.sh COMMAND [ARGS...]' >&2
    exit 2
fi
if [[ "${GITHUB_ACTIONS:-}" == true ]]; then
    exec "$@"
fi
export NEXTEST_TEST_THREADS="${NEXTEST_TEST_THREADS-4}"
if [[ "${LOCAL_SUPERVISED:-}" == 1 ]]; then
    exec "$@"
fi
if [[ "${QUEST_GATE_ADMITTED:-}" == 1 ]]; then
    exec bun --no-orphans "${BASH_SOURCE[0]%/*}/local.ts" "$@"
fi
exec quest gate bun --no-orphans "${BASH_SOURCE[0]%/*}/local.ts" "$@"
