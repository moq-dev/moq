#!/usr/bin/env bash
# Independent shells must reserve different ports even with private temp roots.
set -euo pipefail

lib=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
scratch=$(mktemp -d)
owner=""
trap '[[ -z "$owner" ]] || { kill "$owner" 2>/dev/null || true; wait "$owner" 2>/dev/null || true; }; rm -rf "$scratch"' EXIT
mkdir "$scratch/one" "$scratch/two"
mkfifo "$scratch/ready" "$scratch/release"
export MOQ_TEST_RUNS="$scratch/runs"
unset MOQ_TEST_PORTS

TMPDIR="$scratch/one" bash -euo pipefail -c '
    source "$1/harness.sh"
    harness_begin ports >&2
    harness_port first
    printf "%s\n" "$HARNESS_PORT" >"$2/ready"
    read -r _ <"$2/release"
' bash "$lib" "$scratch" &
owner=$!
read -r first <"$scratch/ready"
TMPDIR="$scratch/two" bash -euo pipefail -c '
    source "$1/harness.sh"
    harness_begin ports >&2
    harness_port second
    if [[ "$HARNESS_PORT" == "$2" ]]; then
        echo "FAIL: separate TMPDIR shells both reserved $2" >&2
        exit 1
    fi
    if harness_port pinned "$2"; then
        echo "FAIL: pinned port ignored another shell reservation" >&2
        exit 1
    fi
' bash "$lib" "$first"
printf 'done\n' >"$scratch/release"
wait "$owner"
owner=""
# Refuse an unexpected owner-controlled target under the predictable root.
ln -s "$scratch/one" "$scratch/ports-link"
MOQ_TEST_PORTS="$scratch/ports-link" bash -euo pipefail -c '
    source "$1/harness.sh"
    harness_begin ports >&2
    if harness_port symlink; then
        echo "FAIL: accepted a symlink reservation root" >&2
        exit 1
    else
        status=$?
        [[ "$status" == 2 ]]
    fi
' bash "$lib"
echo "cross-shell port reservations passed"
