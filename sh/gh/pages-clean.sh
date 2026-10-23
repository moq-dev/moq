#!/usr/bin/env bash
set -euo pipefail

# deploy-pages refuses to choose between artifacts from different run attempts.
# Scope deletion to this run so other Pages publishers keep their artifacts.
: "${GITHUB_REPOSITORY:?}" "${GITHUB_RUN_ID:?}"
artifacts=$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/artifacts?name=github-pages" --jq '.artifacts[].id')
for id in $artifacts; do
    gh api -X DELETE "repos/$GITHUB_REPOSITORY/actions/artifacts/$id"
done
