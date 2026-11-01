# [M] A subscription's range count cannot multiply per-poll cost

## Goal

The model's cost per poll no longer grows with the number of ranges in a
subscription, or that number is bounded by the design (flow control or an
owned budget), not by a refusal constant. The `ranges` bench
(`cargo bench -p moq-net --features fuzz --bench ranges`) sweeps range count
for each path below, so a slope shows up.

## Plan

The fill path went linear in
[moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995).
[moq-dev/moq#7542](https://github.com/moq-dev/moq/pull/7542) keyed the lite-07
frame bounds by group (the security audit's NET-1), split timed datagram
admission into sorted lookups, made a reader's reverse search over its runs
one lookup per leap (rangemap's iterator had walked back from the last run,
which made a sparse feed quadratic), and added the `ranges` bench.

A lite-07 SUBSCRIBE fits about 32K ranges in its 64 KiB message. Three walks
still grow with range count. User-space instructions per step on a pinned
P-core, at 1, 64, 1024, and 16384 ranges, measured on #7542:

| bench | 1 | 64 | 1024 | 16384 |
|---|---|---|---|---|
| `ranges_time_end` | 32K | 228K | 3.2M | 51M |
| `ranges_fills_out` | 46K | 1.5M | 25M | 449M |
| `ranges_sparse` | 24K | 63K | 771K | 14M |

- **Time anchors.** While any time end is unsettled, `Window::settle` advances
  every span on each cursor sync, `Window::rebuild` rebuilds every span's
  groups when one moves, and `Window::watch_starts` and
  `Window::speculative` walk every span on each park. A peer naming many time
  ends past the live edge keeps them unsettled.
  - Open question: indexing the unsettled ends by the sequence each waits on
    does not help that case. Time ranges past the live edge all start at the
    newest group and wait on the same next sequence, so every new group
    reaches every end. One answer: ends whose walks agree share one walk, with
    their times sorted, and a group starting past some of them splits those
    off to settle. Each end then settles once, and a group costs the shared
    walks it reaches.
- **Outstanding fills.** While fills are out, each probe runs
  `Misses::expire` over every wanted run, calling `poll_drifted` per stretch
  (76% of the bench), and `Misses::resolve` collects and polls the joins over
  every wanted run. Expire by deadline as `Deferred` does, and resolve only
  the joins whose outcome changed. Watch for these:
  - A reader's `Waiter` may differ between polls, so skipping a poll because
    nothing changed has to keep one registration that a change will wake.
  - Media staleness ascends with the successor, so the stale stretches form a
    prefix. Wall-clock staleness does not, since groups arrive out of order.
  - An aborted successor hands its stretches to the next live group without
    touching the track.
- **Sparse live numbering.** `TrackState::find_servable` leapfrogs a reader's
  runs and the live groups, so on a track with sparse live sequences
  (moq-transport), a reader with ranges in the gaps costs
  O(min(ranges, gaps)) per search. `live_edge` runs it on every poll.
  - Open question: how to answer the search in one lookup. One answer: each
    reader keeps its own index of the live groups inside its window, updated
    from the arrival ledger on its own poll, as `Misses::judge` already
    tracks it. Stale entries are skipped and dropped when a lookup finds them.

Public API: none expected. Wire: none expected.
