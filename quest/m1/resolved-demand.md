# [M] A widened max age re-offers cached groups

## Goal

A warm relay serves a new reader the same groups a cold one would. Today it
does not: once a reader asking for the oldest group (`Range::oldest`) holds
a copy, its upstream subscription resolved where that reader's max age put
it, say group 10. A newcomer whose wider max age covers groups 5 through 9,
with a floor at 5 or with no floor at all, gets nothing below 10. The relay
does send the wider max age upstream in SUBSCRIBE_UPDATE, but the upstream
cursor never re-offers the cached groups it already skipped as stale. The
reader silently starts later than it asked, and only through a warm relay.

An archive resuming at its last stored frame through a relay that a viewer
already warmed hits this, as does any reader with a wider max age than the
copy's first one.

## Plan

Found while building SUBSCRIBE_ALL
([moq-dev/moq#5931](https://github.com/moq-dev/moq/pull/5931)), which
patched it for listed tracks only by sending the resolved start as an
absolute floor (`listed_demand` in `f074d09e7`).

Decided by the maintainer on 2026-10-19: re-offer when the max age widens,
with no wire change.

- **The aggregate is the union of subscriber ranges**, as
  [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) already decided
  ("Merging subscribers is the union of their ranges"). On a version with a
  single floor, the union of a no-floor reader and a floor F is no floor, so
  the relay sends no floor upstream and filters F locally, which the Rust and
  JS aggregates already do. A floor on the wire
  is a filter (the publisher "MUST NOT deliver an older one"), so sending one
  would drop a late group the no-floor reader is still owed.
- **Why max age, not the floor.** An "oldest" reader resolves at S only
  because the groups below S were stale under its max age, or missing. Age is
  measured against the newest group, so it only grows. A newcomer is owed a
  group below S only if its max age is wider, and a wider max age already
  changes the aggregate (the max of every reader's) and goes upstream in
  SUBSCRIBE_UPDATE. The missing piece is the publisher acting on it.
- **Today a widened max age does not re-offer.** Confirmed in
  `rs/moq-net/src/model/track.rs`: the ordered cursor (`poll_next_group`)
  holds a stale conviction in `seek_pending` and walks it back if the budget
  widens, but only until it delivers a later group, which commits the skip
  and moves `next_sequence` past it for good. The arrival-order path drops
  the gaps past budget from `Delivered` (`forget_past_budget`) and never
  revisits them. Only an uncommitted conviction can come back.
- **The fix.** When a subscriber's max age widens, re-offer the cached groups
  at or above its floor that its cursor gave up as stale and that the new
  budget covers, in the subscription's order: newest first when descending
  (the default, so congestion sheds backlog rather than the live edge), and
  lowest first only when ascending. This extends the re-offer
  [moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929) adds for a
  widened range (a group outside the ranges is parked and re-offered when they
  widen); land after it and reuse that path rather than a second one. Only
  groups this cursor never delivered come back, keeping #5929's rule that
  nothing already served goes out twice; a group already delivered and since
  evicted downstream is a cache miss, which the relay fills upstream by range
  per [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) (a miss below
  the cache becomes a range request), not a re-offer. The same re-offer fires
  at the same max age when a stale verdict loses its edge: the newer group
  that made an older one stale is aborted or evicted, and `TrackState::is_stale`
  revalidates the edge, so a cold cursor would serve the older group.
- **A widening outranks a reported drop**, decided by the maintainer on
  2026-10-19. On lite-05 and lite-06 the publisher reports a stale skip with
  SUBSCRIBE_DROP (lite-07 removed it), and today a reported sequence is
  refused for good, so a re-offer alone would still lose the group.
  - SUBSCRIBE_UPDATE stays incremental and never resets, since a reset races
    in-flight, out-of-order data. A widening obliges the publisher to serve
    the newly covered groups it still holds and never delivered on this
    subscription, including ones it skipped as stale or reported in
    SUBSCRIBE_DROP ("if we grow, we want that data"). Nothing delivered goes
    out twice.
  - A shrink stops serving what it removed without sending drops for it.
  - Data wins over a drop. A drop rides the subscribe stream and groups their
    own streams, so either can land first. A drop means "not coming under the
    old terms": a group arriving after its drop is accepted, and a drop never
    removes a group that already arrived.
  - An evicted group comes back only through a separate past-range request
    (subscribe ranges on lite-07, FETCH before it), never through the live
    subscription, and arrivals are deduped by sequence.

  Code that changes: the Rust lite publisher's `TrackRun::served`
  (`rs/moq-net/src/lite/publisher.rs`) holds served and dropped sequences in
  one set and refuses any group in it, so split dropped from served and gate
  only on served (`a_cut_dropped_then_recreated_is_not_served` flips); the
  same for `#reported` in `js/net/src/track.ts`, whose `#tryRecvGroup` closes
  any group already reported. On the model side, an arrival already clears
  its drop (the insert path in `rs/moq-net/src/model/track.rs`, `#undrop` in
  JS), but `TrackState::drop_groups` and JS `#drop` record the whole range,
  so a drop landing after its group marks a cached group dropped and the
  relay's cursor forwards it; both must skip sequences already cached. The
  subscribers' SUBSCRIBE_DROP handling (`rs/moq-net/src/lite/subscriber.rs`,
  `js/net/src/lite/subscriber.ts`) only accounts the tail, which must keep
  accepting a later stream for an accounted sequence. No wire field and no
  draft change: lite-07 has no SUBSCRIBE_DROP, and lite-05 and lite-06 are
  published, with these rules affecting only behavior.
- **Cost, accepted.** Groups below a newcomer's floor that the wider budget
  covers also come upstream, and the relay drops them locally. Bounded by the
  max age and the cache.
- Rejected: sending min(S, floors) as the wire floor, which filters late
  groups for the no-floor reader; a second upstream SUBSCRIBE bounded to the
  gap, which costs a slot, puts two pulls into one copy, and misses the
  no-floor case; and a new wire hint.

Watch for:

- **Every relay changes.** A widening now costs the publisher a scan of its
  cached groups below the cursor. Keep it proportional to the groups
  re-offered, not the cache, and make sure a reader joining with the same max
  age causes no scan and no SUBSCRIBE_UPDATE. Extend `track_subscriber_churn`
  in `rs/moq-net/benches/track.rs` with widening readers, swept over
  subscriber count and cached groups.
- **`js/net` has the same bug and is in scope.** `combineSubscriptions` in
  `js/net/src/track.ts` already sends no floor when any subscriber has none,
  and its publisher needs the same re-offer on a widened max age. Land a JS
  regression test beside the Rust one.

Test on mock time through a relay: a newcomer with a wider max age, with a
floor below the copy's start and with none, gets the groups upstream still
caches (fails on `main`); the same through a cold relay gives the same
answer; a late group below the copy's start still reaches the no-floor
reader; a reader joining with the same max age sends no SUBSCRIBE_UPDATE; and
a re-offered group is never delivered twice to one subscriber; and a widen,
shrink, evict, widen sequence at the relay gets the evicted groups back
through the range miss fill, not a re-offer; and at the same max age, aborting
or evicting the group that made an older one stale re-offers the older one,
as a cold relay would serve it; on lite-05 and lite-06, a stale group whose
SUBSCRIBE_DROP is flushed before the widening SUBSCRIBE_UPDATE is still
served after it, and the same with the update first; a group arriving after
its drop is accepted, and a drop arriving after its group leaves it served;
a shrink sends no drops. Cover the
warm-relay case in `js/net` too.

Public API: none. Wire: no encoding change; a publisher now re-offers
cached groups when a SUBSCRIBE_UPDATE widens the max age or a stale verdict
loses its edge, including groups it reported in SUBSCRIBE_DROP.

## Related

- [Dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md) - #5929 lands its serving half, including the re-offer this extends
- [Archive producer](/quest/m1/archive/producer.md) - its resume floor crosses relays that viewers may have warmed
