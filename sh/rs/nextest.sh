#!/usr/bin/env bash
set -euo pipefail

# nextest has no environment binding for this flag. Explicit runner options
# take precedence; arguments after `--` are test filters instead.
if [[ -n "${NEXTEST_NO_FAIL_FAST:-}" ]]; then
    case "${1:-} ${2:-} ${3:-}" in
        'cargo nextest run' | 'cargo llvm-cov nextest')
            fail_fast=(--no-fail-fast)
            for arg in "${@:4}"; do
                case "$arg" in
                    --) break ;;
                    --fail-fast | --ff | --no-fail-fast | --nff | --max-fail | --max-fail=* | --no-run)
                        fail_fast=()
                        break
                        ;;
                esac
            done
            set -- "${@:1:3}" "${fail_fast[@]}" "${@:4}"
            ;;
    esac
fi

# Rings share the user's locked-memory accounting across processes. Hosted CI
# can raise its hard limit; local shells use the hard limit the host grants.
if [[ "$(uname -s)" == Linux ]]; then
    if [[ "${GITHUB_ACTIONS:-}" == true && "${RUNNER_ENVIRONMENT:-}" != self-hosted ]]; then
        sudo prlimit --pid "$$" --memlock=67108864
    else
        ulimit -Sl "$(ulimit -Hl)"
    fi
fi
exec bash "$(dirname "$0")/../local.sh" "$@"
