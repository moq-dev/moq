# [S] Remove FETCH-run pacing

## Goal

A moq-transport FETCH run reads its stream at the speed it arrives, as a
lite-07 history run does: no wire request is held at group boundaries for a
slow reader. A reader that falls off the cache tail gets `Evicted`, then
refill-once or a refresh, on both wires.

## Plan

Decided 2026-11-07 (maintainer, /quest-unblock): ✅ no flow control between
groups on either wire (rejected: (a) holding lite-07 group streams unread and
admitting them in the fill's order; (b) narrowing the subscription's ranges
to the next owed group; (c) fetching paced runs a group at a time). Pacing
between groups is head-of-line blocking: under backpressure the head is
evicted and cancels everything queued behind it. SUBSCRIBE is best-effort; per-subscription flow control was considered and not judged worth it.
This deletes the planned lite-07 pacing quest and undoes what
[moq-dev/moq#8117](https://github.com/moq-dev/moq/pull/8117) added for
moq-transport FETCH.

- Remove `track::Fill::poll_run_paced` (`rs/moq-net/src/model/track/fill.rs`)
  and the FETCH stream's `poll_pace` hold that calls it
  (`rs/moq-net/src/ietf/subscriber.rs`), with #8117's tests and any reader or
  test-transport hooks only they use.
- Keep `TrackState::evictions`: `track/miss.rs` reads it too.
- Out of scope: [moq-dev/moq#8017](https://github.com/moq-dev/moq/pull/8017)'s
  pacing of a batch fill's local handler by its fastest reader, which holds
  no wire stream.
- Update the epic README's prose that describes FETCH-run pacing, if any.

Public API: none. Wire: none.

## Related

- [Unbounded groups](/quest/m2/unbounded-groups/README.md) - the epic whose eviction and refill rules a slow reader now falls back on
