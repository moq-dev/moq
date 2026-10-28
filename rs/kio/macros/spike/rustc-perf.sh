#!/usr/bin/env bash
# RUSTC_WRAPPER: run rustc under perf stat, logging "crate,instructions,task-clock-ms".
crate=unknown
prev=
for arg in "$@"; do
	if [ "$prev" = "--crate-name" ]; then crate=$arg; fi
	prev=$arg
done
if [ -z "$PERF_LOG" ] || [ "$crate" = unknown ]; then exec "$@"; fi
out=$(mktemp "$PERF_LOG.XXXXXX")
perf stat -x, -e instructions:u,task-clock -o "$out" -- "$@"
status=$?
# A hybrid CPU reports one instructions line per PMU; sum them. One short
# printf is one atomic append, so parallel rustc runs never interleave.
instr=$(awk -F, '/instructions/ && $1 ~ /^[0-9]+$/ {s += $1} END {printf "%d", s}' "$out")
clock=$(awk -F, '/task-clock/ {print $1; exit}' "$out")
printf '%s,%s,%s\n' "$crate" "$instr" "$clock" >> "$PERF_LOG"
rm -f "$out"
exit $status
