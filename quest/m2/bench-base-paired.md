# [S] just bench BASE uses the paired comparison

## Goal

`just bench <base>` compares through the same paired screen-and-confirm path
the nightly uses, and the old per-case comparison is deleted.

## Plan

[moq-dev/moq#6511](https://github.com/moq-dev/moq/pull/6511) added the paired
path (`--criterion` with `BENCH_BASE`, writing `paired.json`). The BASE
mode in `bench/run.sh` still goes through `compare_criterion`. Route it
through the paired path, and delete `criterion_cases`, `compare_criterion`,
`report_set_changes`, and `--criterion-baseline`. That also deletes the
`criterion_cases ... >file` call that hangs under errexit when it fails.

Public API: none. Wire: none.

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - tunes the thresholds this path uses
