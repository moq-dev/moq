# [S] The binding benchmark's 64 KiB case measures

## Goal

Nightly's `Binding benchmark runners` job passes: the 64 KiB case of the
moq-ffi throughput benchmark (`rs/moq-ffi/src/bench.rs`, run by each
binding's runner such as `py/moq-rs/benches/ffi.py`) completes and reports
a number instead of failing `evicted` on `main`.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- ✅ m0, as a child of [Nightly passes on main](/quest/m0/nightly/README.md)
- m1
- m2

Investigation 2026-11-04:

- [The November 1 binding job](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517685)
  failed in `bench::benchmark_ffi` with `Evicted`.
- [moq-dev/moq#7446](https://github.com/moq-dev/moq/pull/7446), merged later
  that day, fixed the cause in the cache policy: carrying debt for a cold
  victim fell through into shedding the live group's unread frames.
- The [November 2](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408205493)
  and [November 3](https://github.com/moq-dev/moq/actions/runs/41656613559/job/125988511175)
  binding jobs passed. The unchanged benchmark also passes locally at a
  one-second budget on `701b7196a348ff0b9656d7db7a193673772eeb0c`.
- The 64 KiB case writes 4 MiB per round, below its 64 MiB origin budget.
  A local synchronous probe completed 10,000 rounds of every frame size,
  including cache turnover, without eviction. There is no reproduced
  failure that justifies changing the case sizes or cache budget.

Public API: none. Wire: none.

## Waiting

- Maintainer: retire this stale quest (recommended), or retain it with a
  newer failing run or reproducer that the merged cache fix does not
  address. The original failure is fixed and two scheduled runs pass;
  choosing whether to keep the quest changes its scope, so unattended
  investigation leaves that decision for the maintainer.
