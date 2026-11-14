#!/usr/bin/env bash
set -euo pipefail

# Print where moq.dev's vanity tag sends `go get moq.dev/moq-ffi`: `git` for the
# moq-dev/moq-go-ffi mirror, `mod` for the go.moq.dev module proxy. Anything
# else fails, so a release never publishes where the go command doesn't look.
#
# Temporary: once it prints `mod`, the release workflows publish only to
# go.moq.dev, and this script goes with sh/go/publish-ffi-mirror.sh.

meta=$(curl -fsSL 'https://moq.dev/moq-ffi?go-get=1')
imports=$(grep -o '<meta name="go-import" content="[^"]*"' <<<"$meta" | sed 's/.*content="//; s/"$//')
case "$imports" in
    "moq.dev/moq-ffi git https://github.com/moq-dev/moq-go-ffi") echo git ;;
    "moq.dev/moq-ffi mod https://go.moq.dev") echo mod ;;
    *)
        echo "Error: unexpected go-import for moq.dev/moq-ffi: ${imports:-none}" >&2
        exit 1
        ;;
esac
