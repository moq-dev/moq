#!/usr/bin/env bash
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

# Check the staged Go modules, and the Go toolchain they build with, against
# the Go vulnerability database. The committed go/ffi lacks its generated
# bindings and go/wrapper requires a placeholder ffi, so neither type-checks
# until sh/go/stage.sh assembles them from this checkout.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

STAGED=$(bash "$SCRIPT_DIR/stage.sh" --output "$WORKSPACE_DIR/dist")

export CGO_ENABLED=1 GOFLAGS=-mod=mod
status=0
while read -r module; do
    echo "go audit: govulncheck $module"
    (cd "$module" && govulncheck ./...) || status=1
done <<<"$STAGED"
exit "$status"
