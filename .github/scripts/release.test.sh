#!/usr/bin/env bash
# Tests `release.sh ffi-unreleased`, the gate that keeps wrapper releases from
# shipping calls to FFI API that no published moq-ffi has.
set -euo pipefail

release="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

git init -q
git config user.email test@example.com
git config user.name test
git config commit.gpgsign false
git config tag.gpgsign false

mkdir -p rs/moq-ffi py
echo a >rs/moq-ffi/lib.rs
git add . && git commit -qm init
git tag moq-ffi-v0.1.0

expect() {
    local want="$1"
    local version="$2"
    local out="$tmp/output"
    : >"$out"
    GITHUB_OUTPUT="$out" "$release" ffi-unreleased "$version" >/dev/null
    local got
    got=$(cat "$out")
    if [[ "$got" != "unreleased=$want" ]]; then
        echo "ffi-unreleased $version: want unreleased=$want, got '$got'" >&2
        exit 1
    fi
}

expect false 0.1.0

# Changes outside rs/moq-ffi don't affect the published bindings.
echo b >py/wrapper.py
git add . && git commit -qm wrapper
expect false 0.1.0

# A version bumped but not yet tagged means its release is mid-flight.
expect true 0.2.0

# FFI changes merged after the tag, before the next release.
echo b >rs/moq-ffi/lib.rs
git add . && git commit -qm ffi
expect true 0.1.0

echo "release.sh ffi-unreleased: ok"
