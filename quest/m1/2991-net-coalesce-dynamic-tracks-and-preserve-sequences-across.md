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
subscription creates a fresh `track::Request` (`rs/moq-net/src/model/track.rs:3779`),
and `Request::new` creates a fresh `TrackState`. Because `max_sequence`
(`:199`) is empty, both `append_group` (`:1184`) and `append_datagram`
(`:1216`) restart at sequence 0.

That conflicts with the relay's logical track splicing.
`resume::Producer::takeover` (`rs/moq-net/src/model/resume.rs:328`) retains
the previous live edge and starts a replacement at `latest + 1`. Groups from a
restarted producer are therefore filtered until its counter catches up,
causing the same playback stall fixed for JavaScript in #2953.

The takeover tests in `resume.rs` (`takeover_computes_boundary` `:2313`,
`takeover_splices_mid_group` `:3257`,
`takeover_splices_a_replacement_that_resends_the_head` `:3346`,
`takeover_rolls_past_a_finished_group` `:3467`,
`takeover_after_empty_segment_keeps_live_edge` `:3617`) create their
replacement groups with explicit sequences, so none of them exercises
`append_group()` on a restarted producer; using it there would create group 0
and leave the subscriber stalled. Those are the tests to extend. Explicit group
or datagram writes can raise the old producer's shared sequence edge further,
making the catch-up window longer.

### JavaScript permits concurrent same-name dynamic producers

`BroadcastProducer.subscribe()` calls the internal `subscribe` with
`register = false` (`js/net/src/broadcast.ts:49-55`). Multiple publishing-side
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
  (`track.rs:3786`) and re-combines the aggregate whenever a subscriber
  changes (`:3905-3919`).
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
- Rust relay model: extend the `resume.rs` takeover tests above so a
  replacement produced with `append_group()` is delivered immediately rather
  than filtered until catch-up.
- Both implementations: verify a separate broadcast generation starts at 0.

## Closes

- [#2991](https://github.com/moq-dev/moq/issues/2991) - close this issue when the quest finishes
