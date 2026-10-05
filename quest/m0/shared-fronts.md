# [M] Viewer sessions share a front

## Goal

The number of origin fronts for a path is bounded by the peers that appear in
its route chains, not by viewer sessions. Today every session gets
`Hop::random()`, the lite publisher serves through `origin.excluding(hop)`,
and fronts are keyed by `(path, Horizon)` (`model/origin.rs`). Each viewer
therefore mints its own front plus a driver task, and `run_front` only ends
on route retraction or source close, so fronts accumulate for the life of a
long-running broadcast.

## Plan

- Key a front by its effective exclusion: a hop that appears in no route
  chain for the path excludes nothing, so its sessions share the plain front.
  When such a hop later shows up in a chain, the sessions it covers move to
  the filtered front. Decided 2026-09-29, over ending idle fronts after a
  linger, because it also removes the per-viewer cost while viewers are
  active.
- Check whether prefix routes mint fronts for arbitrary covered paths
  (`origin.rs` optimistic resolve) and bound that the same way.
- Measure with a benchmark swept over viewers and broadcasts
  (`rs/moq-net/benches/origin.rs`): fronts and bytes per viewer before and
  after, plus reconnect churn.
- #4799 is this leak, reported by an embedder (decided 2026-10-05 to fold it
  in here, main only, no release backport). Measured on `2704e10e2` with a
  counting allocator: the relay's live heap grows about 15.6 KB and 40
  allocations per session that subscribes to a track, linearly through 16000
  sessions, and survives `malloc_trim`. Connect, announce, and a bare
  `request_broadcast` with no track stay flat. heaptrack attributes it to
  the boxed `run_front` future on the origin's TaskSet and what it owns:
  the front's `tracks` map, its `broadcast::Producer`, request channel and
  track-request table, plus the origin's `fronts` WeakCache and per-path
  watch list. Each dead front is also woken on every route change for its
  path, so the CPU cost grows with sessions ever seen too.
- Add a regression test in `model/origin.rs` with mocked time: N sessions
  request, subscribe to, and drop a track through `excluding(Hop::random())`,
  then the front count returns to baseline. A test is enough for the leak;
  the benchmark above covers cost.

Public API: none. Wire: none.

## Closes

- [#4799](https://github.com/moq-dev/moq/issues/4799) - relay memory grows with subscribers connecting and reconnecting

## Related

- [Front deadlines](/quest/m1/front-deadline-index.md) - per-front cost per track
- [Front parking](/quest/m1/origin-front-parks.md) - also changes what mints a front
- [Wildcard](/quest/m0/wildcard/README.md) - its line rewrites `model/origin.rs` heavily (+401 lines, fronts end on a standing refusal); land after it or rebase onto it
