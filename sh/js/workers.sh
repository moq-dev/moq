#!/usr/bin/env bash
# Validate each Worker's deploy bundle without publishing or credentials.
#
# These Workers have lockfiles outside the Bun workspace, so `just js check`
# never installs or builds them.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

for project in infra/apt infra/rpm demo/pub; do
    (
        cd "$project"
        bun install --frozen-lockfile
        bun run deploy --dry-run
    )
done
