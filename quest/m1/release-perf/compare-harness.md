# [S] Comparison harness

## Goal

`just bench <base>` (`bench/run.sh`) pairs every Criterion case by its full
ID, and writes its report somewhere that survives the worktree. The
[epic](/quest/m1/release-perf/README.md)'s final main against release run
uses it without hand-written joins.

## Plan

`bench/run.sh` discovers cases from `cargo bench -- --list`, which truncates
display titles at 100 characters. That loses long `session_*` IDs. The
2026-10-21 run worked around it by joining the saved `benchmark.json`
`full_id` values instead (`run.sh` already reads them after the run). Discover
and pair cases from those full IDs.

The same run kept its report in another worktree's `.scratch/`, and lost it
when that worktree was removed. Write the report under a path the caller
chooses. Default to the current worktree's `.scratch/`, and print its path at
the end. A durable copy belongs with the epic's final run, not in the repo.
Extend `bench/compare.test.py` to cover a case ID longer than 100 characters.
