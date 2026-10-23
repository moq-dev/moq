#!/usr/bin/env bash
# Audit every independent Bun dependency graph without installing packages.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Every tracked lockfile, so a new Worker can't fall out of the audit.
status=0
for lock in $(git ls-files '*bun.lock'); do
    project=$(dirname "$lock")
    echo "Auditing $project"
    (
        cd "$project"
        bun audit
    ) || status=1
done
exit "$status"
