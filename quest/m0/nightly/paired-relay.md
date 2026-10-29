# [M] Resolve the paired relay-session regressions

## Goal

The reported relay-session regressions are attributed and genuine extra costs are fixed with paired measurements.

## Plan

[Relay session benchmarks](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122851001503) compares
`270395a337` against `22808bbb2f` and flags two lite-07-wip cases:
compression delivery with sixteen publishers +14.5%, and FETCH delivery of
256 frames of 64 bytes +13.3%. Both use one relay, broadcast, and viewer.
The `bench-rust-relay` artifact retains paired samples.

Separate actual work from workload and hosted-runner variation, fix genuine
regressions, and preserve the session workload's delivery checks. Sweep the
fanout dimensions touched by the fix. Threshold calibration remains in
[bench CI](/quest/m1/bench-ci.md).

Public API: none expected. Wire: none expected.
