#!/usr/bin/env bash
# Independent shells must reserve different ports even with private temp roots,
# a relative HARNESS_RUNS must survive a harness changing directory, and a run
# defaults to the checkout's .tmp/ whatever TMPDIR and the working directory say.
set -euo pipefail

lib=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
scratch=$(mktemp -d)
owner=""
shm=""
trap '[[ -z "$owner" ]] || { kill "$owner" 2>/dev/null || true; wait "$owner" 2>/dev/null || true; }; rm -rf "$scratch" "$shm"' EXIT
mkdir "$scratch/one" "$scratch/two"
mkfifo "$scratch/ready" "$scratch/release"
export HARNESS_RUNS="$scratch/runs"
unset HARNESS_PORT_ROOT

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
shm=""
# Refuse an unexpected owner-controlled target under the predictable root.
ln -s "$scratch/one" "$scratch/ports-link"
HARNESS_PORT_ROOT="$scratch/ports-link" bash -euo pipefail -c '
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
# A relative root must still name the same directory after the harness changes directory.
(cd "$scratch" && HARNESS_RUNS=relative bash -euo pipefail -c '
    source "$1/harness.sh"
    harness_begin relative >&2
    [[ "$HARNESS_RUN" == "$2"/relative/relative-* ]] || {
        echo "FAIL: relative HARNESS_RUNS gave run directory $HARNESS_RUN" >&2
        exit 1
    }
' bash "$lib" "$(cd "$scratch" && pwd)")
# Without HARNESS_RUNS a run lives in the checkout, not TMPDIR, and stages a binary
# there as a real file. When /dev/shm is a different filesystem from the checkout,
# the binary comes from it, so the hard link fails and the copy fallback runs.
checkout=$(git -C "$lib" rev-parse --show-toplevel)
fs_of() { df -P "$1" | awk 'NR == 2 { print $1 }'; }
binary_dir="$scratch"
cross=0
if [[ -d /dev/shm && -w /dev/shm && "$(fs_of /dev/shm)" != "$(fs_of "$checkout")" ]]; then
    shm=$(mktemp -d /dev/shm/moq-harness-test.XXXXXX)
    binary_dir="$shm"
    cross=1
fi
printf 'binary' >"$binary_dir/binary"
# shellcheck disable=SC2016  # the script expands in the child shell
(cd "$scratch" && env -u HARNESS_RUNS TMPDIR="$scratch/one" HARNESS_KEEP=1 bash -euo pipefail -c '
    source "$1/harness.sh"
    harness_begin default >&2
    [[ "$HARNESS_RUN" == "$2"/.tmp/harness/default-* ]] || {
        echo "FAIL: default run directory was $HARNESS_RUN, not under $2/.tmp/harness" >&2
        exit 1
    }
    harness_stage bin/binary "$3/binary"
    [[ -f "$HARNESS_RUN/bin/binary" && ! -L "$HARNESS_RUN/bin/binary" && "$(cat "$HARNESS_RUN/bin/binary")" == binary ]] || {
        echo "FAIL: staged binary missing, wrong, or a symlink" >&2
        exit 1
    }
    if (($4)) && [[ "$HARNESS_RUN/bin/binary" -ef "$3/binary" ]]; then
        echo "FAIL: staging across filesystems hard-linked the binary" >&2
        exit 1
    fi
    echo "$HARNESS_RUN" >"$5"
' bash "$lib" "$checkout" "$binary_dir" "$cross" "$scratch/binary.run" 2>"$scratch/default.err") || {
    cat "$scratch/default.err" >&2
    exit 1
}
rm -rf "$(cat "$scratch/binary.run")"
[[ -z "$(ls -A "$scratch/one")" ]] || {
    echo "FAIL: the default run wrote into TMPDIR" >&2
    exit 1
}
echo "cross-shell port reservations passed"
