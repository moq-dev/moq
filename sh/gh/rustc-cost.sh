#!/usr/bin/env bash
# RUSTC_WRAPPER for the nightly gate cost scenarios (sh/gh/gate-cost.sh): runs
# each compilation under `perf stat` and appends one tab-separated line to
# $RUSTC_COST: package, manifest directory, target kind, CPU milliseconds, and
# user-space instructions. perf counts the linker and every other child too.
# Cargo's probes name no crate and run untouched.
set -euo pipefail

: "${RUSTC_COST:?names the file that collects each compilation cost}"

crate=
kind=lib
previous=
for arg; do
    case "$previous" in
        --crate-name) crate=$arg ;;
        --crate-type) [[ "$arg" != bin ]] || kind=bin ;;
    esac
    [[ "$arg" != --test ]] || kind="test"
    previous=$arg
done
if [[ -z "$crate" || "$crate" == ___ ]]; then
    exec "$@"
fi
[[ "$crate" != build_script_* ]] || kind=build

stats=$(mktemp)
trap 'rm -f "$stats"' EXIT
status=0
perf stat -x, --no-big-num -e instructions:u,task-clock -o "$stats" -- "$@" || status=$?

# A hybrid CPU reports instructions per core type, and a type the compiler
# never ran on is "<not counted>".
cpu=
instructions=
while IFS=, read -r count _ event _; do
    case "$event" in
        task-clock*) cpu=${count%.*} ;;
        *instructions*) [[ ! "$count" =~ ^[0-9]+$ ]] || instructions=$((${instructions:-0} + count)) ;;
    esac
done <"$stats"
if [[ -z "$cpu" || -z "$instructions" ]]; then
    echo "rustc-cost: perf counted no CPU time or instructions for $crate:" >&2
    cat "$stats" >&2
    exit 1
fi
printf '%s\t%s\t%s\t%s\t%s\n' "${CARGO_PKG_NAME:-$crate}" "${CARGO_MANIFEST_DIR:-}" "$kind" "$cpu" "$instructions" >>"$RUSTC_COST"
exit "$status"
