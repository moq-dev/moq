# Main is no slower than release

## Goal

Before the next release cut, every benchmark where `main` is slower than
`release` is either fixed or accepted with a recorded reason. Accidental cost
is fixed. Cost that pays for a feature `main` deliberately added is accepted,
and the reason is written here.

This epic's own work: once every child has landed, run the full main against
release comparison again with `just bench origin/release`, plus the JS frame
path in Chromium with `bench/browser/micro.ts`, and confirm nothing new
regressed. Then add a release-baseline step to the recurring
[performance audit](/quest/m2/audit-perf.md), so every later cut
repeats this comparison.

## Plan

Decided 2026-10-21, when planning from a paired comparison of `release`
(`0c56e5b4e`, which included the bench backport
[#6103](https://github.com/moq-dev/moq/pull/6103)) against `main`
(`8b0d9952f`). The run used 811 paired Criterion cases, three alternating
confirmation rounds for the largest moves, JS on bun, and the relay load
harness. It ran on a shared 8-core host, so a small wall-time change may be
noise.

- The epic gates the cut through the [m1 milestone](/quest/m1/README.md),
  because `main` must not ship slower than `release` without a decision.
- In scope: the five cases that held through confirmation, plus the large
  screen-only moves. The screen-only moves are re-measured with callgrind
  instruction counts, and any that hold are fixed.
- Only accidental cost is fixed. Cost from an intended feature is accepted,
  and the reason is recorded here.
- The IETF `/fetch` rows of `session_delivery_group_cost` are accepted and
  dropped from release pairing. They measure different work on each branch:
  release's bare track handle opened a live SUBSCRIBE, while main sends a real
  FETCH per viewer ([#5820](https://github.com/moq-dev/moq/pull/5820) made
  handles lazy, and [#4974](https://github.com/moq-dev/moq/pull/4974) added
  fetch-only demand). The lite `/fetch` cost was real: a relay sent one
  upstream FETCH per group, but revived the parked logical track for every
  viewer's FETCH. [#6286](https://github.com/moq-dev/moq/pull/6286) serves a
  whole cached group from the parked copy instead, cutting 12 to 19% of the
  instructions per viewer where viewers share a group. The rest of the gap to
  a viewer that holds a query is accepted: each viewer's own fetch of a group
  it lacks revives its logical track, so the open group keeps its route and
  fails over mid-read, and its copy releases the route's demand once idle.
- `stats/session-report` is accepted at O(live sessions), after a cheap fix.
  Per-label payload bytes ([#5731](https://github.com/moq-dev/moq/pull/5731))
  match payload billing per label, such as per signing key. After removing
  redundant row locking and live-session CAS, three alternating measurements
  give 52.2 us for 10,000 sessions without wires and 388.4 us with one real
  transport wire each. A once-per-second drain uses about 0.04% of a core
  with those wires; the earlier under-0.01% estimate omitted their walk.
  This remaining per-session and per-connection sampling is accepted. Deleting
  per-label bytes was rejected.
- `track_gc/idle` is accepted at about 20 ns, flat across track counts, after
  [#6291](https://github.com/moq-dev/moq/pull/6291) skipped the clock read
  with no wall deadline and reused the bench's waiter (36.7 ns before). The
  remaining ~8 ns over release's 11.6 ns is the lock and driver registration
  that the wall-clock aging from
  [#5548](https://github.com/moq-dev/moq/pull/5548) needs. An earliest-deadline
  atomic cannot prove a replacement driver is registered, and a lock-free
  notification primitive is not worth ~8 ns.
- `track_gap_flush` is accepted at O(log gaps + retired entries) per shared
  successor, after [#6287](https://github.com/moq-dev/moq/pull/6287) indexed
  the gap ledger and retired a shared verdict's gaps with one split. Each
  retired entry must be freed exactly once, so the batch is already amortized
  optimal; a representation that hides that reclamation would widen scope for
  no measured benefit. Sparse arrivals cost O(log gaps) amortized, and at 4096
  gaps flush, read, and expiry instructions fell 80 to 95%.

### Release as a baseline

`release` lacks many of the features the backported benches exercise:
per-reader delivery records, wall-clock aging, the gap ledger, rewind checks,
and epochs. So a ratio against `release` shows where to look, not what to fix.
Main's own sweeps over a size parameter are the better regression signal: a
cost that grows with the sweep is accidental, while a flat per-operation cost
usually pays for a feature.

### Confirmed (three rounds, ratio is main / release)

| Bench | release | main | Ratio | Owner |
|---|---|---|---|---|
| `track_gap_flush/4096` | 5.7 us | 551 us | 96x | accepted (above) |
| `track_gap_flush/512` | 1.8 us | 48 us | 27x | accepted (above) |
| `track_gap_flush/64` | 1.5 us | 6.2 us | 4.1x | accepted (above) |
| `stats/session-report/1000x10` | 11.0 us | 86.8 us | 7.9x | accepted (above) |
| `track_gc/idle/16384` | 11.6 ns | 73.7 ns | 6.3x | accepted (above) |
| `origin/resume_overlap/sequence/1r_64g_32c` | 93 us | 426 us | 4.6x | fixed in [moq-dev/moq#6292](https://github.com/moq-dev/moq/pull/6292) |
| `session_delivery_group_cost/moq-transport-22/.../viewers=64/.../fetch` | 1.19 ms | 3.86 ms | 3.2x | accepted (above) |
| `group_write_frames/single/8192` | 424 us | 486 us | 1.17x | [Re-check](/quest/m1/release-perf/recheck.md) |

Main is much faster on subscriber churn, joins, range demand, and idle
sessions (by 100x to 3000x). The relay load test used 21% less CPU on video
and 12% less on fanout, with the same delivery.

### Screen only (one run, unconfirmed)

- moq-mux: `container-export/ts/4x1` 4.7x and `mkv/4x1` 3.9x (median 1.41x
  over 27 cases); `annexb-import-{bytes,frames}` median 2.1x (up to 3.6x).
- moq-net track: `track_stamp_groups` median 2.7x (4.7x at `sparse/4096`),
  `track_sparse_read/timed/4096` 4.3x, `track_datagram_subscriber_join`
  median 2.3x, `track_parallel_write` 1.6x, `track_fanout_frame` 1.42x,
  `track_empty_run` median 1.36x.
- moq-net origin and session: `origin/announce_duplicate/30d_1s` 4.3x,
  `session_delivery_relays` median 1.56x, `paced` 1.45x, `scale` 1.43x.
- moq-stats `aggregate` at live=16, departed=4096: 2.1x. The other rows were
  1.0 to 1.3x, so this one is probably noise.
- kio `contended_notify` and `contended_log` at 16 threads: 4 to 5x. These
  are contention benches on a busy host, so they are likely noise.
- JS `net/bench/frames.ts` on bun: lite rows 1.2 to 1.3x, IETF rows 1.3 to
  2.2x. Fixed in Chromium:
  [#6296](https://github.com/moq-dev/moq/pull/6296) removed a retained-history
  scan, and [#6479](https://github.com/moq-dev/moq/pull/6479) stopped
  releasing a finished stream's reader lock, which built a `TypeError` per
  group stream. Every row then sits within the A/A spread of release.
  The group write/read microbench stays 3 to 12% above release against an
  A/A of 3 to 8% on the same host. Decided 2026-10-23 (maintainer): browser
  parity accepted on this evidence; the final re-run above checks it again.

The raw report lived in a scratch directory that has since been removed.
`just bench <base>` now pairs full Criterion IDs; point `MOQ_BENCH_ARTIFACTS`
outside the worktree to keep the next one.

## Required

- [Re-check](/quest/m1/release-perf/recheck.md) - the screen-only moves are re-measured with instruction counts, and the ones that hold are fixed or accepted

## Related

- [Performance audit](/quest/m2/audit-perf.md) - compares nightly to nightly; this epic compares main to release
- [Nightly benchmark regressions](/quest/m1/bench-ci.md) - paired nightly-to-nightly comparison on one runner
