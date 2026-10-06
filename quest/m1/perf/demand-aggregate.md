# [M] Track demand aggregates without a scan

## Goal

A track subscribe, leave, or preference update costs the same no matter how
many readers the track has. Today each one wakes the publisher, which re-folds
every subscription and re-arms a waiter on each (`combined_subscription` in
`rs/moq-net/src/model/track.rs`). Since #4922 viewers share a front, so one hot
track pays this per join and per leave: `viewer_join/1000v_1b` went from 18 µs
to 158 µs, about 1.5 ms per join at 10k readers.

Rust only (decided 2026-10-06): no JS relay fans one track out to thousands of
readers, so `js/net/src/track.ts` keeps its fold.

## Plan

- Shared state, no message passing (decided 2026-10-06). `Subscriptions`
  becomes per-field counted ordered maps for `priority`, `max_age`, `start`,
  and `end`, with a count for the `None` arm (no floor, unbounded) that absorbs
  `start` and `end`. Subscribe, update, and drop mutate it directly under the
  lock: remove the subscriber's old contribution, add the new one. Each
  subscriber keeps its own last value for that. The per-subscriber
  `kio::Consumer<Subscription>` channels, the waiter re-arming, and the
  departed-pruning pass go away.
- Wake the publisher manually, and only when the combined value changes (the
  head of a field's map moves), not on every subscriber change. kio is
  optional here.
- `snapshot_subscription` reads the combined value without a walk. The
  publisher's retention clamp (`clamp_combined`) still applies on read.
- Bench first, in `rs/moq-net/benches/track.rs` (decided 2026-10-06): make
  `track_subscriber_join` and `track_subscriber_churn` measure per-operation
  cost swept over the readers already on the track (1, 100, 1k, 10k), and add a
  preference-update sweep over the same axis, including a leave by the reader
  that holds a field's extreme. `viewers.rs` `viewer_join` is the end-to-end
  check.
- A unit test checks the counted aggregate against a full fold across random
  subscribe, update, and drop sequences.

Public API: none expected. Wire: none.

## Related

- [Front deadlines](/quest/m1/front-deadline-index.md) - the per-track half of a front's cost; this is the per-reader half
- [Subscribe ranges](/quest/m1/subscribe-ranges/model.md) - changes what the aggregate holds; whichever lands second adapts
