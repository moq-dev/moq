# [XS] moq-json's table/deltas bench measures the same work every iteration

## Goal

`moq-json`'s `table/deltas/1%/{10000,20000}` cases report a stable time, so
their Nightly trend history is usable. Today each iteration ticks one
long-lived encoder, so a short run either catches a forced snapshot or misses
it: 4.9 vs 35.7 ms, and 71 vs 10 ms, in
[moq-dev/moq#7928](https://github.com/moq-dev/moq/pull/7928)'s quick screen.

## Plan

Planned 2026-11-05 (maintainer, /quest-plan of the follow-ups from the
2026-11-05 /quest-complete run), from #7928. Milestone: ✅ m1.

Make each iteration do the same work: measure deltas and forced snapshots as
separate cases, or give every iteration the same snapshot cadence. Check the
fix with repeated quick screens of both cases on one host.

Public API: none. Wire: none.

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - its rounds rejected these cases as outliers
