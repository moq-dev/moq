#!/usr/bin/env bash
# Prove a poll-only library build compiles no coroutines: no `async fn` body,
# `async` block, or `async` closure in the crate's own MIR. A library's `async`
# feature carries its wrappers, so a build without it should have none. Name a
# PACKAGE by its `Cargo.toml` path when another dependency shares its name.
#
# usage: coroutines.sh PACKAGE [-- CARGO_ARGS...]
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

usage='usage: coroutines.sh PACKAGE [-- CARGO_ARGS...]'
if [[ $# == 0 || ($# -ge 2 && "$2" != --) ]]; then
    echo "$usage" >&2
    exit 2
fi
package=$1
shift
if [[ $# != 0 ]]; then
    shift
fi

cd "$(git rev-parse --show-toplevel)"
mir=$(mktemp)
trap 'rm -f "$mir"' EXIT

# Only the final crate gets `--emit`, so its dependencies build as usual.
if [[ $package == */Cargo.toml ]]; then
    select=(--manifest-path "$package")
else
    select=(-p "$package")
fi
cargo rustc --locked "${select[@]}" --lib "$@" -- --emit "mir=$mir"

# rustc names each coroutine by its body, as `{async fn body of queue::Queue<T>::pop()}`.
found=$(grep -oE '\{async (fn body of [^{}]*|block@[^{}]*|closure@[^{}]*)\}' "$mir" | sort -u || true)
if [[ -n "$found" ]]; then
    echo "error: $package compiles coroutines without its async feature:" >&2
    echo "$found" >&2
    echo "Gate each behind the feature, or generate it from a poll with #[kio::wrapper]." >&2
    exit 1
fi
echo "$package: no coroutines"
