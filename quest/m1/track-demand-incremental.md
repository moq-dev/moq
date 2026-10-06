# [M] Track demand updates in constant time

## Goal

A subscribe or leave on a track costs the same whether the track has one
reader or ten thousand. Today every change re-sums demand over all of a
track's readers.

## Plan

Shared fronts (#4922) put every viewer of a path on one track, which exposed
the cost: `origin/viewer_join/1000v_1b` went from 18 us to 158 us while churn
and memory improved. The aggregate (subscription ranges, max delay, priority,
whatever the subscription carries) must update with bounded work independent
of reader count on every subscribe and leave, including when the departing
reader supplied an aggregate extreme. Optimizing only dominated readers does
not meet this goal. If the aggregate makes that bound infeasible, bring back
the measured tradeoff for a maintainer decision before narrowing the goal.

- Benchmark joins and leaves, including aggregate-extreme departures, swept
  over readers per track and tracks, using the
  `rs/moq-net/benches/viewers.rs` target #4922 adds.
- Check the JS model for the same pattern and mirror the fix if it applies.
- Coordinate with [Subscribe ranges](/quest/m1/subscribe-ranges/model.md),
  which changes what the aggregate is; whichever lands second adapts.

## Required

- [Shared fronts](/quest/m0/shared-fronts.md) - #4922 provides the shared track fan-out and viewer benchmark
