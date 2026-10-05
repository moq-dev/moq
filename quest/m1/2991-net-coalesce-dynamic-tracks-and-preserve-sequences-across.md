# [M] net: coalesce dynamic tracks and preserve sequences across replacements

## Goal

A broadcast has one logical dynamic track per name in both languages: at most
one pending or live producer, one on-demand request that every subscriber fans
out from, and a group and datagram sequence namespace that survives the
producer being replaced. Behavior only, so it ships on main.

## Plan

Dynamic tracks should have one logical identity per broadcast and track name,
but the Rust and JavaScript models violate different parts of that invariant.

### Rust resets sequences when a dynamic producer is replaced

A closed dynamic track is removed from the broadcast's weak cache. The next
subscription creates a fresh `track::Request` (`rs/moq-net/src/model/track.rs`),
and `Request::new` creates a fresh `TrackState`. Because its `max_sequence`
is empty, both `append_group` and `append_datagram` restart at sequence 0.

That conflicts with the relay's logical track. The pump this was written
against is gone: #4741 resumes route changes by reading the routes' copies
(`rs/moq-net/src/model/front.rs`, `resume.rs`, and `origin.rs`). Re-check
first that a replacement restarting at sequence 0 still stalls under
copy-based resume, the way groups from a restarted producer were skipped
until its counter caught up (the playback stall fixed for JavaScript in
#2953), and rewrite this section against what the code does now. With #4741 this is the general hazard of restarting at 0 under one
name, which [broadcast epochs](/quest/m0/broadcast-epoch/README.md) fix for a
restarted publisher. This quest covers what an epoch does not: one dynamic
track replaced inside a live broadcast.

The route-change tests (`rs/moq-net/tests/route_change.rs`) are the ones to
extend: none replaces a producer through `append_group()`, which would
create group 0 and, if the stall still holds, leave the subscriber stalled. Explicit group or datagram
writes can raise the old producer's shared sequence edge further, making the
catch-up window longer.

### JavaScript permits concurrent same-name dynamic producers

`BroadcastProducer.subscribe()` calls the internal `subscribe` with
`register = false` (`js/net/src/broadcast.ts`). Multiple publishing-side
subscriptions for the same name therefore enqueue independent requests and
create independent `track.Producer` instances.

#2953 made those concurrent producers share a sequence allocator. That
prevents duplicate sequence allocation, but concurrent producers are the wrong
model. Publishing-side subscriptions should coalesce like
`BroadcastConsumer.subscribe()` and Rust's `broadcast::Consumer::track`: one
pending or live producer per broadcast and name, one on-demand request, and
multiple subscribers fanning out from it.

### Desired behavior

- A broadcast has at most one pending or live dynamic track producer per track
  name.
- Concurrent JavaScript publishing-side subscriptions for the same name emit
  one request and share its accepted producer.
- Subscription options from all subscribers remain aggregated on that request.
  `track::Request` already does this in Rust: it carries `prev_subscription`
  and re-combines the aggregate whenever a subscriber changes.
- After that producer closes, a later request creates a new producer but
  continues the group and datagram sequence namespace for that broadcast and
  name.
- Explicit group and datagram writes advance the shared allocator.
- A new broadcast generation starts each track at sequence 0.
- Rust and JavaScript expose the same lifecycle and sequencing behavior.

The sequence allocator is shared across producer incarnations without sharing
the closed producer's cache or terminal state.

### Regression coverage

- JavaScript: two `BroadcastProducer.subscribe()` calls for one name produce
  one request and both subscribers receive from the accepted producer.
- JavaScript: remove or replace #2953's concurrent-producer test, since
  concurrent same-name producers should not be representable.
- Rust and JavaScript: close a dynamic producer after group and datagram
  sequences have advanced, re-request the same name, and verify the
  replacement appends at the next sequence.
- Rust relay model: extend the route-change tests above so a replacement
  produced with `append_group()` is delivered immediately rather than skipped
  until catch-up.
- Both implementations: verify a separate broadcast generation starts at 0.

## Closes

- [#2991](https://github.com/moq-dev/moq/issues/2991) - close this issue when the quest finishes
