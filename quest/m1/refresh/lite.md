# [M] Refresh on the moq-lite wire

## Goal

On lite-07, a track's refresh ratio reaches every subscriber in
TRACK_INFO. A join at any relay that crosses the ratio reaches the origin
publisher's `Demand::wanted`, through a relay cluster, in Rust and JS. A
peer on an older lite version sees a track that never refreshes.

## Plan

Decided 2026-10-16 (see the [epic](/quest/m1/refresh/README.md)).

- **TRACK_INFO** gains a `Refresh` varint: 0 for none, otherwise ratio + 1,
  matching how lite-07 encodes Publisher Max Age. lite-07 only, while it is
  the wip version. This quest gates the
  [lite-07 cut](/quest/m1/lite07-finalize.md), because missing it would push
  core-message fields to lite-08.
- **SUBSCRIBE needs no field.** A new SUBSCRIBE is itself a join at the
  upstream hop, which evaluates the ratio against its own cache.
- **SUBSCRIBE_UPDATE** gains a `Wanted Group` varint: 0 for none, otherwise
  sequence + 1. A relay's upstream driver sends it when its track's
  `Demand::wanted` passes the largest group. It sends nothing more until the
  largest group reaches it, so redundant downstream joins coalesce at every
  hop. Clear it with the subscription, so a reconnect inherits nothing.
- **Receiving it** counts as a join under the ratio at that hop, like a
  downstream IETF NEW_GROUP_REQUEST. A stale value at or below the largest
  group is ignored.
- **js/net** encodes and decodes both fields and feeds its `Demand::wanted`,
  so a browser publisher refreshes. A browser subscriber's SUBSCRIBE is its
  join.
- **Drafts.** Update `drafts/draft-lcurley-moq-lite.md` (TRACK_INFO,
  SUBSCRIBE_UPDATE, the lite-07 changelog) and pass `just drafts check`.
- **The ietf extension.** Cluster hops no longer need it to carry
  dynamic-group requests; native lite does. This quest removes that duty
  from the [relay extension](/quest/m1/ietf-extension/lite.md) if that quest
  has not landed yet, or deletes its request path if it has.

Tests: codec round trips on lite-07 and an older version without the
fields. A two-relay cluster test checks that a join over the ratio at the far
relay reaches the origin's `wanted`, a join under it does not, and
concurrent joins send one SUBSCRIBE_UPDATE. Use simulated time. Run
`just test interop --all`.

Public API: none beyond `Demand::wanted` and `track::Info::refresh`. Wire: two
new lite-07 fields.
