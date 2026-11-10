#!/usr/bin/env bash
# Reject a MIGRATE.md edit outside the release cut. A breaking PR puts its
# migration note in its Impact section; the cut gathers those notes into each
# package's guide. The cut is recognized by its edit adding a version-transition
# heading (`## 0.3 -> 0.4`) to that guide, which a diff shows in the merge queue too.
#
# Usage: sh/migrate.sh [BASE]
#
# BASE resolves as in sh/changed.sh.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

base=$(sh/changed.sh --merge-base "${1:-}")

edited=()
while IFS= read -r -d '' path; do
    if ! git diff --no-renames -U0 "$base" -- "$path" | grep -qE '^\+## .+ -> .+'; then
        edited+=("$path")
    fi
done < <(git diff --name-only --no-renames -z "$base" -- ':(glob)**/MIGRATE.md')

if ((${#edited[@]})); then
    echo "error: only the release cut edits MIGRATE.md, but the branch edits:" >&2
    printf '       %s\n' "${edited[@]}" >&2
    echo "       revert them and put the migration note in the PR's Impact section instead" >&2
    exit 1
fi
