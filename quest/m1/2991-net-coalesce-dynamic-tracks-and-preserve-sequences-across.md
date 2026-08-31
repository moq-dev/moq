# [M] net: coalesce dynamic tracks and preserve sequences across replacements

## Goal

Implement and verify the behavior tracked in [#2991](https://github.com/moq-dev/moq/issues/2991)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Dynamic tracks should have one logical identity per broadcast and track name, but the Rust and JavaScript models currently violate different parts of that invariant.

##### Rust resets sequences when a dynamic producer is replaced

A closed dynamic track is removed from the broadcast's weak cache. The next subscription creates a fresh `track::Request`, and `Request::new` creates a fresh `TrackState`. Because `max_sequence` is empty, both `append_group` and `append_datagram` restart at sequence 0.

That conflicts with the relay's logical track splicing. `resume::Producer::takeover` retains the previous live edge and starts a replacement at `latest + 1`. Groups from a restarted producer are therefore filtered until its counter catches up, causing the same playback stall fixed for JavaScript in #2953.

The existing `test_linger_reconnect_splices` avoids the reset by explicitly creating replacement group 2 after the old producer emitted groups 0 and 1. Using `append_group()` there would create group 0 and leave the subscriber stalled. Explicit group or datagram writes can raise the old producer's shared sequence edge further, making the catch-up window longer.

##### JavaScript permits concurrent same-name dynamic producers

`BroadcastProducer.subscribe()` calls the internal subscribe path with `register = false`. Multiple publishing-side subscriptions for the same name therefore enqueue independent requests and create independent `track.Producer` instances.

\#2953 made those concurrent producers share a sequence allocator. That prevents duplicate sequence allocation, but concurrent producers are the wrong model. Publishing-side subscriptions should coalesce like `BroadcastConsumer.subscribe()` and Rust's `broadcast::Consumer::track`: one pending or live producer per broadcast and name, one on-demand request, and multiple subscribers fanning out from it.

#### Desired behavior

- A broadcast has at most one pending or live dynamic track producer per track name.
- Concurrent JavaScript publishing-side subscriptions for the same name emit one request and share its accepted producer.
- Subscription options from all subscribers remain aggregated on that request.
- After that producer closes, a later request creates a new producer but continues the group/datagram sequence namespace for that broadcast and name.
- Explicit group and datagram writes advance the shared allocator.
- A new broadcast generation starts each track at sequence 0.
- Rust and JavaScript expose the same lifecycle and sequencing behavior.

The sequence allocator should be shared across producer incarnations without sharing the closed producer's cache or terminal state.

#### Regression coverage

- JavaScript: two `BroadcastProducer.subscribe()` calls for one name produce one request and both subscribers receive from the accepted producer.
- JavaScript: remove or replace #2953's concurrent-producer test, since concurrent same-name producers should not be representable.
- Rust and JavaScript: close a dynamic producer after group/datagram sequences have advanced, re-request the same name, and verify the replacement appends at the next sequence.
- Rust relay model: keep a logical subscriber live across replacement and verify the first replacement group is delivered immediately rather than filtered until catch-up.
- Both implementations: verify a separate broadcast generation starts at 0.

## Closes

- [#2991](https://github.com/moq-dev/moq/issues/2991) - close this issue when the quest finishes
