# [S] moq-net's pool-debt flight test holds its bound in parallel runs

## Goal

`pool_debt_sheds_a_flight_without_its_cached_group` in
`rs/moq-net/src/model/track.rs` keeps the pool under its `2 * POOL` bound
when it shares a process with the rest of moq-net's tests, fixed at its
cause under the [epic](/quest/m0/test-flakes-2/README.md)'s rules, without
loosening the bound. Its sibling `pool_debt_sheds_a_flight` failed beside it
in two of the runs below, so check it against the same cause. Done when the
PR explains the overshoot and shows the parallel run that reproduced it now
passing.

## Plan

Added 2026-11-05 (maintainer, /quest-complete follow-ups). The test landed
in [moq-dev/moq#7944](https://github.com/moq-dev/moq/pull/7944) (merged
2026-11-05 18:36Z). No CI or merge-queue run has failed it: CI runs nextest,
one process per test, and it passed there in every run checked (for example
[run 42104077314](https://github.com/moq-dev/moq/actions/runs/42104077314)).
Every failure came from land fixers running plain `cargo test`, where tests
share one process on parallel threads. Each passed alone, with
`--test-threads=1`, and under nextest:

- [moq-dev/moq#7937](https://github.com/moq-dev/moq/pull/7937)
  ([note](https://github.com/moq-dev/moq/pull/7937#issuecomment-6667877930),
  2026-11-05 19:58Z): `frame 763: 8402340 bytes held`.
- [moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670)
  ([note](https://github.com/moq-dev/moq/pull/7670#issuecomment-6668545954),
  20:30Z): both pool-debt tests failed in parallel. Then
  ([note](https://github.com/moq-dev/moq/pull/7670#issuecomment-6670516495),
  21:52Z) `cargo test --locked -p moq-net -p moq-archive --all-features`
  failed twice: `frame 500: 8402340 bytes held`, then
  `frame 523: 8402372 bytes held`.
- [moq-dev/moq#7951](https://github.com/moq-dev/moq/pull/7951)
  ([note](https://github.com/moq-dev/moq/pull/7951#issuecomment-6670771027),
  22:06Z): `frame 500: 8402340 bytes held`. It failed again in that PR's
  later validation
  ([note](https://github.com/moq-dev/moq/pull/7951#issuecomment-6674925814)).
- [moq-dev/moq#7988](https://github.com/moq-dev/moq/pull/7988)
  ([note](https://github.com/moq-dev/moq/pull/7988#issuecomment-6671969800),
  23:12Z, and again at
  [00:05Z](https://github.com/moq-dev/moq/pull/7988#issuecomment-6672860745)
  and [01:07Z](https://github.com/moq-dev/moq/pull/7988#issuecomment-6673858792)):
  both pool-debt tests failed in parallel, holding about 8.4 MB.

The overshoot is always about 13.7 KB past the 8,388,608-byte bound, less
than one of the test's frames (8 MiB is about 500 of them).

Lead, unverified: the pool ranks groups for eviction by a coarse 100 ms
wall-clock access tick (`TICK_MS` in `rs/moq-net/src/model/cache.rs`),
breaking ties within a tick with small write and read boosts. A thread
slowed by its neighbours spreads the test's writes across tick boundaries.
That could change which group pays the flight's debt, leaving one frame
unpaid. Before trusting this lead, rule out process-wide state shared
between tests in one binary, since the failures need parallel threads.

Reproduce first: run plain `cargo test -p moq-net --all-features` (not
nextest) on parallel threads until it fails, and record the frame and bytes
held. Then fix the cause. If the test depends on wall time, drive the pool's
clock as the epic's rules say. If the shed logic leaves debt unpaid when the
writes straddle a tick, fix the shed logic.

Public API: none. Wire: none.
