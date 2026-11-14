#!/usr/bin/env bash
set -euo pipefail

# Publish the staged moq.dev/moq-ffi module to the go.moq.dev module proxy as
# v${BUILD_VERSION}, lockstep with the moq-ffi crate: build its GOPROXY files,
# upload them to the go-moq-dev bucket, then download the version through
# moq.dev's vanity tag, which also has sum.golang.org record its hash.
#
# Required environment:
#   BUILD_VERSION  - version string (e.g. 0.4.27)
#   R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY - unless --dry-run; see infra/go/r2.sh
#
# Flags:
#   --dry-run  Build the GOPROXY files and print their go.sum lines, uploading
#              nothing.
#
# Expects the staged Go module tarball under `go-out/`, produced by
# package-ffi.sh as `moq-ffi-${BUILD_VERSION}-go.tar.gz`. The version's time is
# the checked-out commit's, so a rerun rebuilds identical files, which the
# upload skips.

DRY_RUN=false
while [[ $# -gt 0 ]]; do
    case $1 in
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        -h | --help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

: "${BUILD_VERSION:?BUILD_VERSION is required}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA_DIR="$(cd "$SCRIPT_DIR/../../infra/go" && pwd)"
VERSION="v${BUILD_VERSION}"

TARBALL="$PWD/go-out/moq-ffi-${BUILD_VERSION}-go.tar.gz"
[[ -f "$TARBALL" ]] || {
    echo "Error: missing $TARBALL" >&2
    exit 1
}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

tar -xzf "$TARBALL" -C "$WORK"
STAGED="$WORK/moq-ffi-${BUILD_VERSION}-go"
[[ -d "$STAGED" ]] || {
    echo "Error: tarball did not contain $STAGED" >&2
    exit 1
}

SUMS=$(GOWORK=off go -C "$SCRIPT_DIR/modzip" run . \
    -dir "$STAGED" -version "$VERSION" \
    -time "$(git -C "$SCRIPT_DIR" log -1 --format=%cI)" \
    -out "$WORK/proxy")
echo "$SUMS"
ls -l "$WORK/proxy/moq.dev/moq-ffi/@v"

if [[ "$DRY_RUN" == true ]]; then
    echo "Dry-run: not uploading."
    exit 0
fi

"$INFRA_DIR/upload.sh" "$WORK/proxy"

# The path a `GOPROXY=direct` user takes, and the first fetch of this version
# through proxy.golang.org, which sum.golang.org then pins.
GOT=$("$INFRA_DIR/verify.sh" --proxy direct "$VERSION")
WANT=$(head -1 <<<"$SUMS")
if [[ "$GOT" != "$WANT" ]]; then
    echo "Error: moq.dev/moq-ffi@$VERSION resolves to a different module than was uploaded:" >&2
    echo "  uploaded: $WANT" >&2
    echo "  resolved: $GOT" >&2
    exit 1
fi
echo "Published moq.dev/moq-ffi@$VERSION to go.moq.dev: $GOT"
