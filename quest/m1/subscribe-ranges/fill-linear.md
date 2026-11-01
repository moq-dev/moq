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
which made a sparse feed quadratic), shared the walks of time anchors, and
added the `ranges` bench.

- **Time anchors: shared walks.** ✅ Decided 2026-11-01 (maintainer):
  unsettled time ends whose walks agree (same `known`, `held`, speculative
  group, and cap) share one walk, holding their times in sorted order. A
  group starting at time `t` splits off the ends before `t`, and each of
  those settles. Ends that converge merge. Each end settles once, and
  `rebuild` stops being O(spans) per move. Done in #7542: `ranges_time_end`
  is flat in range count.
  - Rejected: indexing the unsettled ends by the sequence each waits on,
    since time ranges past the live edge all wait on the same next sequence;
    and charging ranges against request flow control, a wire change.

A lite-07 SUBSCRIBE fits about 32K ranges in its 64 KiB message. Three
paths still grow with range count. User-space instructions per step on a
pinned P-core, at 1, 64, 1024, and 16384 ranges, measured on #7542:

| bench | 1 | 64 | 1024 | 16384 |
|---|---|---|---|---|
| `ranges_fills_out` | 46K | 1.5M | 25M | 449M |
| `ranges_sparse` | 24K | 63K | 771K | 14M |

`ranges_time_cap` was added after that measurement. It takes 4.1 µs,
15.6 µs, 388 µs, and 8.2 ms per step at the same counts (criterion, pinned
P-core).

- **Window moves.** A move costs every fixed run, so a time end whose cap
  moves on each group multiplies the fixed ranges beside it (`ranges_time_cap`).
  The cap moves twice per group: a group that arrives yet to start caps the end,
  and its start lifts the cap. Each move does all of these:
  - `Window::rebuild` copies the fixed runs.
  - `Cursor::update_servable` rebuilds the `Servable` and compares the
    `Published` runs.
  - `Window::covered_by` restarts from the floor.
  - `Misses` rebuilds the wanted runs from `Window::missable` for the new
    revision.
  - Open question: how a move can cost only the runs that moved. One answer:
    keep the fixed runs shared, built once per placement and grown as ends
    settle, with the walks' few runs layered over them. Then `Servable`,
    `Published`, `covered_by`, and `Misses` each carry or diff only the layer.

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
  - Time ends have the same shape here. Ends left behind different missing
    sequences of a sparse feed can't merge, so a settle still advances
    O(min(ranges, gaps)) walks. Whatever answers the search can also wake
    only the walks a change reaches.

Public API: none expected. Wire: none expected.
