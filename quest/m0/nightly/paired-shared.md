# [M] Resolve the paired catalog fanout regressions

## Goal

The reported catalog fanout regressions are attributed and genuine extra costs are fixed with paired measurements.

## Plan

[Rust shared bench targets](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122851001460) compares
`270395a337` against `22808bbb2f` and flags four catalog-estimate fanout cases:
Hang one track/four readers +11.2%, one track/sixteen readers +20.1%, four
tracks/sixteen readers +12.2%, and MSF references one track/sixteen readers
+16.4%. The `bench-rust-shared` artifact retains paired samples.

Determine whether the cost is production work, a changed workload, or an
invalid comparison. Fix genuine regressions and measure the publisher and
subscriber axes rather than only one fanout point. Coordinate comparison
calibration with [bench CI](/quest/m1/bench-ci.md).

Public API: none expected. Wire: none expected.
