# [M] Halve the slowest tests' CPU

## Goal

The 103 test IDs frozen in [moq-dev/moq#6914](https://github.com/moq-dev/moq/pull/6914)
(the slowest `ceil(10,275 / 100)` of one full normal `cargo nextest` run) take under
68.92 CPU-seconds together, half their 137.83 s baseline, with every case and its
coverage preserved. Per-test CPU is the test process's user plus system time, not
nextest's wall-clock duration. Measure the same IDs on the same machine as the baseline
it is compared against; missing or failed cases are not imputed.

## Plan

Decided with the maintainer on 2026-10-26, from moq-dev/rust2's
[test selection report](https://github.com/moq-dev/rust2/pull/69): the slowest 1%
of tests make up 57% of what either package selection or a function-level selector
would run, so cutting them beats building a selector.

- Prefer making a test cheaper: smaller fixtures, shared setup, simulated time instead
  of real waits, fewer redundant cases.
- Moving a test to nightly only follows this epic's rule: evidence and an explicit
  coverage tradeoff for each.

### Measurement

[moq-dev/moq#6914](https://github.com/moq-dev/moq/pull/6914) holds the frozen list with
per-test baseline and after CPU, plus the runner wrapper and command. The wrapper is a
`CARGO_TARGET_<triple>_RUNNER` that records each nextest test child's `wait4` user plus
system time, run under the normal `just rs test --workspace --exclude moq-net-fuzz
--exclude moq-quic-fuzz` with zero retries. On a different machine, re-measure the
baseline source (`6f6a3ae21432a3df74d4c432451f3909da339bee`) and the candidate with the
same wrapper rather than comparing against these numbers. Unchanged tests varied
between runs on a shared host (moq-stats' `produce::tests` by up to 40%), so credit only
the tests a change touches.

### Done so far

[moq-dev/moq#6914](https://github.com/moq-dev/moq/pull/6914) cut the cohort from
137.83 s to 116.83 s with fixture-only changes: the two MPEG-TS reset tests in
`container::ts::import` drain on the exporter's deadlines with a retention derived from
their stimulus instead of an hour of idle output (12.98 s to 0.18 s), and the jitter
`drift` and `feed` helpers advance a `kio::clock::Deadlines` queue instead of the tokio
paused clock (10.16 s to 6.80 s across 8 tests). About 48 s remains to cut.

Tried and dropped, so don't redo them without new evidence:

- Caching the jitter lead median: the frame-rate benchmark stayed within noise and one
  cell regressed about 6%.
- A containment shortcut for miss-verdict intersections in moq-net: sparse gap and
  range sweeps were unchanged (4096 timed gaps, 2.796 ms to 2.783 ms). The broader
  algorithmic fix landed in
  [moq-dev/moq#6921](https://github.com/moq-dev/moq/pull/6921).
- Retaining only one phase's span in the TS reset tests: it shed the boundary and
  failed the unsignalled-reset assertion.

### Remaining cost

After CPU of the frozen cohort by module, largest first:

| Module | Tests | CPU s |
| --- | ---: | ---: |
| moq-mux `container::ts::export_timing_test` | 14 | 22.2 |
| moq-net `model::track::test` | 5 | 17.3 |
| moq-mux `container::ts::recovery_test` | 18 | 11.6 |
| moq-net integration `mesh_withdraw` | 6 | 9.7 |
| moq-net integration `routing_contracts::handoff` | 3 | 8.3 |
| moq-mux `jitter::tests` | 8 | 6.8 |
| moq-quic `tests::sim` | 3 | 4.6 |
| moq-stats `produce::tests` | 4 | 4.0 |
| everything else | 42 | 32.4 |

The moq-net `model::track::test` gap tests run through the same miss judging, which
[moq-dev/moq#6921](https://github.com/moq-dev/moq/pull/6921) made incremental after this
measurement, so re-measure them first; the export timing and recovery tests are the
largest fixture-only target.

Public API: none.
