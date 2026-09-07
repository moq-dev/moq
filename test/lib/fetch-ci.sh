#!/usr/bin/env bash
# Download the debug bundles a CI run uploaded, so the same failure a reviewer
# is looking at can be opened locally.
#
#     ./fetch-ci.sh 12345678901
#     ./fetch-ci.sh https://github.com/moq-dev/moq/actions/runs/12345678901
#
# Read-only: it views the run and downloads its artifacts, and does nothing
# else. Bundles land under target/qa/ci-<run id>/ next to the local ones.
set -euo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)

if [[ $# -ne 1 || -z "$1" ]]; then
    echo "usage: fetch-ci.sh <run id or run url>" >&2
    exit 2
fi

command -v gh >/dev/null 2>&1 || {
    echo "error: gh not found; install the GitHub CLI to fetch CI bundles" >&2
    exit 1
}

# Accept the URL as pasted from a check's "Details" link, not only the bare id.
RUN="${1##*/runs/}"
RUN="${RUN%%/*}"
[[ "$RUN" =~ ^[0-9]+$ ]] || {
    echo "error: could not read a run id out of '$1'" >&2
    exit 2
}

OUT="${MOQ_QA_ARTIFACTS:-$WORKSPACE/target/qa}/ci-$RUN"
mkdir -p "$OUT"

# The run's own verdict first: an artifact from a cancelled or still-running job
# is a partial capture, and reading it as a finished failure wastes the trip.
gh run view "$RUN" --json displayTitle,headBranch,status,conclusion,workflowName \
    --template '{{.workflowName}} on {{.headBranch}}: {{.status}} / {{.conclusion}}{{"\n"}}{{.displayTitle}}{{"\n"}}'

# The bundles are uploaded under a qa-bundle-* name; everything else in the run
# (coverage, build output) is somebody else's artifact.
if ! gh run download "$RUN" --pattern 'qa-bundle-*' --dir "$OUT"; then
    echo "error: no qa-bundle-* artifacts on run $RUN" >&2
    echo "       a passing job uploads none, and retention expires them" >&2
    exit 1
fi

echo
echo "bundles: $OUT"
find "$OUT" -name manifest.json -maxdepth 3 | sed 's/^/  /'
