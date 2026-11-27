#!/usr/bin/env bash
# Deploy an uploaded Pages artifact to moq's GitHub Pages site by its ID.
#
#   sh/gh/pages-deploy.sh ARTIFACT_ID
#
# actions/deploy-pages finds the artifact by listing the run's artifacts by name, and
# that listing can miss an upload finalized seconds earlier
# (https://github.com/actions/deploy-pages/issues/451). upload-pages-artifact already
# returns the ID, so this deploys it directly and nothing waits on the listing.
# Needs `pages: write` and `id-token: write`; writes page_url to GITHUB_OUTPUT.
set -euo pipefail

if [[ $# != 1 || ! "$1" =~ ^[0-9]+$ ]]; then
    echo 'usage: pages-deploy ARTIFACT_ID' >&2
    exit 2
fi
: "${GITHUB_REPOSITORY:?}" "${GITHUB_SHA:?}" "${GITHUB_OUTPUT:?}"
: "${ACTIONS_ID_TOKEN_REQUEST_URL:?needs id-token: write}" "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:?needs id-token: write}"

oidc=$(curl -fsS -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" "$ACTIONS_ID_TOKEN_REQUEST_URL" | jq -er .value)
echo "::add-mask::$oidc"

deployment=$(
    jq -n --argjson artifact "$1" --arg version "$GITHUB_SHA" --arg oidc "$oidc" \
        '{artifact_id: $artifact, pages_build_version: $version, oidc_token: $oidc}' |
        gh api -X POST "repos/$GITHUB_REPOSITORY/pages/deployments" --input -
)
id=$(jq -er .id <<<"$deployment")
page_url=$(jq -er .page_url <<<"$deployment")
echo "page_url=$page_url" >>"$GITHUB_OUTPUT"
echo "Deploying artifact $1 to $page_url"
deployment_path="repos/$GITHUB_REPOSITORY/pages/deployments/$id"

# A cancelled or timed-out job stops its deployment rather than leave it pending.
trap 'gh api -X POST "$deployment_path/cancel" >/dev/null; exit 1' INT TERM

# Pages offers no completion event, so poll as deploy-pages does; the job's timeout bounds it.
# A failed read is not a result: the deployment exists and may still be settling.
while :; do
    sleep 5
    status=$(gh api "$deployment_path" --jq .status) || status=unavailable
    case $status in
        succeed) exit 0 ;;
        deployment_failed | deployment_content_failed | deployment_cancelled | deployment_lost)
            echo "::error::Pages deployment ended with $status"
            exit 1
            ;;
        *) echo "Pages deployment status: $status" ;;
    esac
done
