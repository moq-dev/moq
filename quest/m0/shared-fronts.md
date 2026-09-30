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

Public API: none. Wire: none.

## Related

- [Front deadlines](/quest/m1/front-deadline-index.md) - per-front cost per track
- [Front parking](/quest/m1/origin-front-parks.md) - also changes what mints a front
