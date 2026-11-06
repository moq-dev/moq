#!/usr/bin/env bash
# Validate each Worker's lockfile and deploy bundle, and run its tests, without
# publishing or credentials.
#
# These Workers have lockfiles outside the Bun workspace, so `just js check`
# never installs or builds them. Every tracked lockfile but the workspace's is
# one, so a new Worker can't fall out of the check; infra/mbx only drives
# wrangler's CLI and has no bundle to build.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

for lock in $(git ls-files '*bun.lock'); do
    project=$(dirname "$lock")
    [[ "$project" == . ]] && continue
    (
        cd "$project"
        bun install --frozen-lockfile
        if [[ -f wrangler.jsonc ]]; then
            bun run deploy --dry-run
        fi
        tests=(src/*.test.ts)
        if [[ -e "${tests[0]}" ]]; then
            bun test
        fi
    )
done
