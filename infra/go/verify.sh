#!/usr/bin/env bash
set -euo pipefail

# Download versions of moq.dev/moq-ffi through a module proxy from a clean
# module cache, so the go command checks each against sum.golang.org, and print
# their go.sum lines.
#
# Usage: infra/go/verify.sh [--proxy URL] [VERSION...]
#
#   --proxy URL  GOPROXY to read through, default https://go.moq.dev. `direct`
#                follows moq.dev's vanity tag, as `GOPROXY=direct` users do.
#
# Without versions, it checks every version the proxy lists, and fails if the
# proxy lacks any version proxy.golang.org lists.

MODULE=moq.dev/moq-ffi
PROXY=https://go.moq.dev
VERSIONS=()
while [[ $# -gt 0 ]]; do
    case $1 in
        --proxy)
            PROXY="$2"
            shift 2
            ;;
        -h | --help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        -*)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
        *)
            VERSIONS+=("$1")
            shift
            ;;
    esac
done

WORK=$(mktemp -d)
trap 'chmod -R u+w "$WORK"; rm -rf "$WORK"' EXIT
export GOMODCACHE="$WORK/mod" GOFLAGS=-modcacherw GOWORK=off
export GOPROXY="$PROXY" GOSUMDB=sum.golang.org GONOSUMDB= GOPRIVATE= GONOSUMCHECK= GOINSECURE=

if [[ "${#VERSIONS[@]}" -eq 0 ]]; then
    served=$(go list -m -versions "$MODULE" | cut -d' ' -f2- | tr ' ' '\n')
    expected=$(curl -fsSL "https://proxy.golang.org/$MODULE/@v/list")
    missing=$(grep -vxF -f <(printf '%s\n' "$served") <<<"$expected" || true)
    if [[ -n "$missing" ]]; then
        echo "Error: $PROXY lacks versions proxy.golang.org lists:" >&2
        printf '  %s\n' $missing >&2
        exit 1
    fi
    # A read loop, not mapfile: macOS ships bash 3.2.
    while IFS= read -r v; do VERSIONS+=("$v"); done <<<"$served"
fi

for v in "${VERSIONS[@]}"; do
    sum=$(go mod download -json "$MODULE@$v" | jq -r .Sum)
    echo "$MODULE $v $sum"
    chmod -R u+w "$GOMODCACHE"
    rm -rf "${GOMODCACHE:?}/$MODULE@$v" "$GOMODCACHE/cache/download/$MODULE"
done
