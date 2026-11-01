#!/usr/bin/env bash
# Prove a poll-only library build compiles no coroutines: no `async fn` body,
# `async` block, or `async` closure in the crate's own MIR. A library's `async`
# feature carries its wrappers, so a build without it should have none. Each
# `--allow NAME` excuses the `async fn NAME` bodies still waiting on a poll
# conversion, and fails once none is left, so the list only shrinks.
#
# usage: coroutines.sh PACKAGE [--allow NAME]... [-- CARGO_ARGS...]
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

usage='usage: coroutines.sh PACKAGE [--allow NAME]... [-- CARGO_ARGS...]'
if [[ $# == 0 ]]; then
    echo "$usage" >&2
    exit 2
fi
package=$1
shift
allowed=()
while [[ $# != 0 ]]; do
    case "$1" in
        --allow)
            [[ $# -ge 2 && "$2" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || {
                echo "$usage" >&2
                exit 2
            }
            allowed+=("$2")
            shift 2
            ;;
        --)
            shift
            break
            ;;
        *)
            echo "$usage" >&2
            exit 2
            ;;
    esac
done

cd "$(git rev-parse --show-toplevel)"
mir=$(mktemp)
trap 'rm -f "$mir"' EXIT

# Only the final crate gets `--emit`, so its dependencies build as usual.
cargo rustc --locked -p "$package" --lib "$@" -- --emit "mir=$mir"

# rustc names each coroutine by its body, as `{async fn body of queue::Queue<T>::pop()}`,
# sometimes without the module path, so an allowed name matches as the last segment.
found=$(grep -oE '\{async (fn body of [^{}]*|block@[^{}]*|closure@[^{}]*)\}' "$mir" | sort -u || true)
unexpected=$found
for name in ${allowed[@]+"${allowed[@]}"}; do
    pattern="^\{async fn body of ([A-Za-z0-9_<>, ]+::)*$name[<(]"
    if ! grep -qE -- "$pattern" <<<"$found"; then
        echo "error: $package no longer compiles async fn $name; remove its --allow" >&2
        exit 1
    fi
    unexpected=$(grep -vE -- "$pattern" <<<"$unexpected" || true)
done
if [[ -n "$unexpected" ]]; then
    echo "error: $package compiles coroutines without its async feature:" >&2
    echo "$unexpected" >&2
    echo "Gate each behind the feature, or generate it from a poll with #[kio::wrapper]." >&2
    exit 1
fi
echo "$package: no coroutines beyond ${#allowed[@]} allowed async fn names"
