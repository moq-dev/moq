#!/usr/bin/env bash
set -euo pipefail

# Rings share the user's locked-memory accounting across processes. Hosted CI
# can raise its hard limit; local shells use the hard limit the host grants.
if [[ "$(uname -s)" == Linux ]]; then
    if [[ "${GITHUB_ACTIONS:-}" == true && "${RUNNER_ENVIRONMENT:-}" != self-hosted ]]; then
        sudo prlimit --pid "$$" --memlock=67108864
    else
        ulimit -Sl "$(ulimit -Hl)"
    fi
fi
exec "$@"
