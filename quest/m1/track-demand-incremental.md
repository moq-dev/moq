# [M] Track demand updates in constant time

## Goal

A subscribe or leave on a track costs the same whether the track has one
reader or ten thousand. Today every change re-sums demand over all of a
track's readers.

## Plan

Shared fronts (#4922) put every viewer of a path on one track, which exposed
the cost: `origin/viewer_join/1000v_1b` went from 18 us to 158 us while churn
and memory improved. The aggregate (subscription ranges, max delay, priority,
whatever the subscription carries) needs to update incrementally, or be
structured so the common case (a reader joining or leaving with a dominated
subscription) does not walk the others.

- Benchmark swept over readers per track and tracks, using the
  `rs/moq-net/benches/viewers.rs` target #4922 adds.
- Check the JS model for the same pattern and mirror the fix if it applies.
- Coordinate with [Subscribe ranges](/quest/m1/subscribe-ranges/model.md),
  which changes what the aggregate is; whichever lands second adapts.
