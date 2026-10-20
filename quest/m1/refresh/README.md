# Refresh: new groups on demand

## Goal

A publisher opts a track in to refresh with a byte ratio R. When a new
subscriber joins and the current group's bytes after its first frame exceed
R times that first frame, the publisher is asked for a fresh group, at the
origin and through every relay hop. Groups no longer roll on a fixed
schedule: a joiner never downloads more than about R + 1 times a snapshot or
keyframe, and existing subscribers stop paying for refreshes nobody needed.

Boundaries:

- The criterion is transfer size, not time. A joiner only costs a refresh
  once the deltas outweigh the first frame, so refreshes rate-limit
  themselves without a timer.
- Joining is the only trigger. There is no explicit subscriber request; a
  decoder that lost sync rereads the cached group from its start.
- Non-goals: flate modes, json stream mode, and importers that cannot force
  a keyframe (CMAF, TS).

## Plan

Decided with the maintainer on 2026-10-16.

- **Why not IETF semantics as-is.** DYNAMIC_GROUPS is a boolean and
  NEW_GROUP_REQUEST carries only a group ID, with no rate limit beyond
  "outstanding until Largest Group grows". Reviewers of the draft asked for
  a way to say "a recent group is fine". A ratio advertised by the publisher
  lets every hop decide that locally from its cache, without app knowledge.
- **A ratio, not a duration.** A time minimum was considered first and
  rejected: a fresh json snapshot every second costs every subscriber a full
  group, which can be worse than the deltas. Transfer size is the thing
  being optimized. A subscriber-weighted rule (refresh when bytes exceed
  (N + 1) times the first frame) was rejected because N is per-hop fanout,
  so relays would disagree and the publisher could not advertise it.
- **One demand state.** `track::Demand::wanted() -> Option<u64>` replaces
  `used()` and `unused()`: `None` while unused, `Some(n)` when consumers want
  group `n` or later. A producer opens a group whenever `n` passes its
  largest. The same signal covers the first subscriber, resuming a paused
  encoder, and a refresh. It was preferred over keeping `used()` beside a
  new event, which would leave two ways to say the same thing.
- **Names.** `track::Info::refresh: Option<u32>` is the ratio, in the same
  unit as moq-json's `delta_ratio`, which it replaces. `None` means joins
  never trigger groups. It is the only home of the ratio: producers that
  receive a created track read it from there.
- **Order.** Relay requests for dynamic groups landed first (#5708) as
  relay-internal IETF support. The core quest makes its private request
  path public, so the work is promoted rather than redone.
- **m1.** Replacing `Demand` is a breaking API change, and m1 finishes the
  planned breaking APIs before the cut.

Work owned by this README, after the children land:

- An end-to-end test: a moq-json snapshot track published through a
  two-relay cluster, where a late joiner at the far relay receives a fresh
  snapshot group once the deltas exceed the ratio, and no fresh group while
  they don't. Use simulated time and wire it into CI.
- A short "Refresh" section on the `doc/concept` page that covers groups:
  what the ratio means, what a publisher does on `wanted`, and how relays
  forward it. No new guide.

## Required

- [Lite wire](/quest/m1/refresh/lite.md) - lite-07 TRACK_INFO advertises the ratio and SUBSCRIBE_UPDATE carries the wanted group, so a join at any relay reaches the origin
- [moq-json](/quest/m1/refresh/json.md) - snapshot and window groups roll on a qualifying join, reading the ratio from the track; `delta_ratio` and `op_ratio` become `deltas` and `ops` switches in every binding
- [Video](/quest/m1/refresh/video.md) - video publishers advertise refresh and force a keyframe on demand, keeping the encoder's max GOP

## Related

- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - its dynamic-groups child is the IETF half this epic promotes
- [JSON window timestamps](/quest/m1/json-window-timestamps.md) - touches the same window consumers
