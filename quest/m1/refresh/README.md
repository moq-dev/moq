# [M] Refresh: new groups on demand

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
- Non-goals: json stream mode and importers that cannot force a keyframe
  (CMAF, TS).

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
- **Order.** Relay requests for dynamic groups (#5708) were promoted to
  the public `Demand::wanted` and `track::Info::refresh` in #5930, and
  #6030 carries refresh demand through lite relays. The moq-json producers
  now answer that demand.
- **m1.** Replacing `Demand` is a breaking API change, and m1 finishes the
  planned breaking APIs before the cut.

The remaining work is the Goal's download-byte bound. Demand propagation and
the JSON answer work, but a join still receives the cached group while its
request travels upstream. With R=1, a 14-byte snapshot and 64 deltas totaling
503 bytes, a late reader across two relays can download all 517 cached bytes
before the JSON owner answers, rather than about 28 bytes. This was reproduced
with simulated time; it is a counterexample, not behavior to preserve.

The normal integration gate now exercises a real moq-json snapshot producer
and consumer through a publisher link, two clustered relays, and a reader
link. It checks ratios 1 and 2 below, exactly at, and above the threshold;
elapsed time without a join; a fresh full snapshot under a new group number
at the newest source timestamp; and subsequent deltas. It tolerates a stale
prefix without requiring one. The concept page documents the current soft
trigger and its byte-bound limitation.

Before changing serving behavior, settle how a pending refresh ends:

- **Recommended: preserve the Goal's bound.** Withhold an oversized cached
  prefix for that live join, with explicit terminal behavior when the source
  finishes, aborts, or cannot produce the requested group. Do not silently
  fall back to the oversized prefix. A warm Rust cursor could start beyond
  the current group, but that alone does not cover cold caches that learn
  the ratio or size late, or a resume front selecting another route copy.
- **Alternative: accept only a soft trigger.** Keep serving cached content
  while requesting a fresh group. This explicitly changes the settled Goal
  and drops its R+1 download claim.

Implement the chosen contract in Rust and matching JS per-reader serving,
without changing existing readers' content or timestamps. Define and test
warm and cold caches, late track info and first-frame size, finish/abort
before the answer, and route-copy replacement while a refresh is pending.
Keep the wire draft and concept docs accurate if withholding becomes part
of the contract. A demand-only fix is insufficient: the current draft says
to request a fresh group, without promising to withhold cached delivery.

## Related

- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - its dynamic-groups child is the IETF half this epic promotes
