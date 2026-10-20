#!/usr/bin/env bash
# Audit every independent Bun dependency graph without installing packages.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

status=0
for project in . infra/apt infra/rpm infra/moq-sh demo/pub; do
    echo "Auditing $project"
    (
        cd "$project"
        bun audit
    ) || status=1
done
exit "$status"
