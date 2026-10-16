# Regression net for moq-quic

## Goal

`moq-quic` has benchmarks, fuzzing, characterization tests, and a seeded
simulation dense enough that later QUIC work can refactor aggressively and
see every behavior or cost change as a failing test or a benchmark delta.

The net pins today's behavior; it does not change it. A bug the net finds
lands as an `#[ignore]`d failing test naming a new follow-up quest, never as
an inline fix.

## Plan

Decided in the 2026-10-16 plan with the maintainer:

- **Baseline is `origin/main` at b30289127**, before the rewrites
  (#5711 scheduler, #5690 reliable reset, #5735 receive memory, #5688
  shard, #5674 peer limits) merge. #5690 and #5688 merged while this was
  planned, so they take the merged-first comparison in
  [the benchmarks](/quest/m1/quic/coverage/bench.md) and the
  characterization tests are written against b30289127, then rebased. The net lands first so each rewrite can
  compare against it: benchmarks via `just bench` against the commit that
  lands them (Criterion only compares cases present at the base), tests by
  rebasing, where every behavior change shows up as a failing test to
  accept or fix.
- **Ranked first in the QUIC epic, but blocks nothing.** Every m1 QUIC
  rewrite already has an open PR, and `Required` gates starting, not
  merging. The net mainly protects the next wave of changes.
- **Benchmarks and characterization tests drive the public
  `Connection`/`Endpoint` API** through the test `Pair`, so they compile on
  both sides of a rewrite. Internal unit tests may break with a refactor and
  get rewritten there.

Survey on the baseline (462 tests, 1.4 s): 88.1% of production lines run,
but behavior is thin where the rewrites cut. RESET_STREAM is received 4
times in the whole suite, STREAMS_BLOCKED_BIDI never,
`set_max_concurrent_streams` and `RecvStream::received_reset` are 0%,
`endpoint.rs` is 86.5%, frame error paths are unexercised, upstream
quinn's fuzz targets were never imported, there are no crate benchmarks,
and the harness defaults to Cubic while production runs BBR3.

## Required

- [Benchmarks](/quest/m1/quic/coverage/bench.md) - Criterion sweeps of stream scheduling, receive and reset, endpoint routing, and the wire codec, on the baseline
- [Coverage report](/quest/m1/quic/coverage/report.md) - `just rs coverage <crate>` and a nightly lcov artifact, with no threshold
- [Characterization tests](/quest/m1/quic/coverage/characterize.md) - golden scheduling order, and every uncovered frame, reset, limit, and endpoint path, on today's harness
- [Fuzzing](/quest/m1/quic/coverage/fuzz.md) - a moq-net-style fuzz crate with the upstream parser targets, frame decode, and a connection op-sequence target, run nightly
- [Seeded simulation](/quest/m1/quic/coverage/sim.md) - a deterministic, impaired network in `Pair`, BBR3 as the harness default, and property tests over random workloads

## Related

- [Receive memory coverage](https://github.com/moq-dev/moq/pull/5752) - the end-to-end moq-tokio receive-window benchmark; the crate benchmarks here sit below it
- [Delete NewReno](/quest/m1/quic/new-reno.md) - the dead controller the survey found at 0%
