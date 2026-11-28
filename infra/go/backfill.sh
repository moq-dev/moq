#!/usr/bin/env bash
set -euo pipefail

# Copy every version of moq.dev/moq-ffi that proxy.golang.org lists, and the
# bucket lacks, into the go-moq-dev bucket, then build any tag of the
# moq-dev/moq-go-ffi git mirror that neither holds: a release the mirror took
# after moq.dev's cutover, which proxy.golang.org can no longer reach.
#
# Usage: infra/go/backfill.sh
#
# Environment: see r2.sh.
#
# `go mod download` checks each version against sum.golang.org, and its
# cache/download directory is already the GOPROXY layout. The files are copied
# byte for byte, so the h1 hashes the checksum database pinned still match.
# A version whose .info the bucket has is left alone: proxy.golang.org may zip
# one published here differently, which upload.sh would refuse.

MODULE=moq.dev/moq-ffi
MIRROR=https://github.com/moq-dev/moq-go-ffi
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/go/r2.sh
source "$HERE/r2.sh"

WORK=$(mktemp -d)
trap 'chmod -R u+w "$WORK"; rm -rf "$WORK"' EXIT

versions=$(curl -fsSL "https://proxy.golang.org/$MODULE/@v/list" | sort -V)
have=$(rclone lsf -R --files-only --include "/$MODULE/@v/*.info" "$BUCKET" | sed 's|.*/||; s|\.info$||')

export GOMODCACHE="$WORK/mod" GOFLAGS=-modcacherw GOWORK=off
export GOPROXY=https://proxy.golang.org GOSUMDB=sum.golang.org GONOSUMDB='' GONOPROXY='' GOPRIVATE='' GOINSECURE=''

out="$WORK/out/$MODULE/@v"
mkdir -p "$out"
copied=0
for v in $versions; do
    if grep -qxF "$v" <<<"$have"; then
        continue
    fi
    go mod download "$MODULE@$v"
    for ext in info mod zip; do
        cp "$GOMODCACHE/cache/download/$MODULE/@v/$v.$ext" "$out/"
    done
    # Only the cache/download files are uploaded; drop the extracted tree to bound disk use.
    chmod -R u+w "$GOMODCACHE/$MODULE@$v"
    rm -rf "$GOMODCACHE/$MODULE@$v"
    copied=$((copied + 1))
done

# A mirror tag proxy.golang.org does not list is a release only the mirror
# took, after moq.dev's cutover. sum.golang.org has pinned no hash for it, so
# modzip builds it from the tag's tree, the files proxy.golang.org would have
# zipped.
tags=$(git ls-remote --tags --refs "$MIRROR" 'v*' | sed 's|.*refs/tags/||' | sort -V)
built=0
for v in $tags; do
    if grep -qxF "$v" <<<"$versions" || grep -qxF "$v" <<<"$have"; then
        continue
    fi
    src="$WORK/src/$v"
    git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$v" "$MIRROR" "$src"
    at=$(git -C "$src" log -1 --format=%cI)
    rm -rf "$src/.git"
    GOWORK=off go -C "$HERE/../../sh/go/modzip" run . -dir "$src" -version "$v" -time "$at" -out "$WORK/out"
    rm -rf "$src"
    built=$((built + 1))
done

echo "backfill: $copied of $(wc -w <<<"$versions") versions to upload, and $built mirror-only tags"
[[ "$((copied + built))" -gt 0 ]] || exit 0
"$HERE/upload.sh" "$WORK/out"
