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
Groups the upstream already reported dropped stay out of scope here (see
[Drop set](/quest/m1/drop-set.md)).

An archive resuming at its last stored frame through a relay that a viewer
already warmed hits this, as does any reader with a wider max age than the
copy's first one.

## Plan

Found while building SUBSCRIBE_ALL
([moq-dev/moq#5931](https://github.com/moq-dev/moq/pull/5931)), which
closed unmerged; its listed-track workaround (sending the resolved start as
an absolute floor) never reached main.

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
  [moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929) landed for a
  widened range (a group outside the ranges is parked and re-offered when they
  widen); reuse that path rather than a second one. Only
  groups this cursor never delivered come back, keeping #5929's rule that
  nothing already served goes out twice; a group already delivered and since
  evicted downstream is a cache miss, which the relay fills upstream by range
  per [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) (a miss below
  the cache becomes a range request), not a re-offer.
- **Only a widening reverses a stale verdict,** a bigger budget or a wider
  range, decided by the maintainer on 2026-10-19 ("Narrow, then merge").
  Nothing at the same terms re-offers a group the cursor gave up, so no
  cursor tracks what convicted it.
- **Open question: warm/cold equivalence after an edge loss is not
  promised.** When the newer group that made G stale is aborted or evicted,
  a cold cursor may serve G while a warm one stays past it. Deferred
  because watching for it needs more than this plan carries: a verdict
  depends on two groups (`drifted` uses G's successor for `reach` and a
  possibly later presentation edge in `live_above`, and losing either can
  free G), a range may end mid-group so an edge needs a frame position, and
  a run skipped at the max age bound would no longer be final. Likewise,
  a stale drop in flight across an aggregate range change (lite-07, see
  [Drop set](/quest/m1/drop-set.md)) may settle a reader whose own range
  would not convict, since SUBSCRIBE_UPDATE has no acknowledgement and the
  drop names only its `Max Age`; deferred for the same reason. A group
  arriving after its drop was folded into a cursor's floor under pool
  pressure is not delivered to that cursor ([Drop set](/quest/m1/drop-set.md)).
- **Each cursor keeps a disposition record,** as the maintainer first framed
  it ("keep what was dropped within each cursor and within the track
  state"). It is what makes a re-offer safe: the cursor must tell a group it
  gave up as stale (re-offer it) from one it delivered (never resend it).
  The record is one run-length map per cursor, each run marked delivered,
  skipped as stale (with the budget and range it was judged under), or
  reported in SUBSCRIBE_DROP (final for now, below). It replaces today's `Delivered`
  (`rs/moq-net/src/model/delivered.rs`), whose floor (`forget_below`) folds
  stale gaps in with delivered groups, and absorbs `TrackRun::served` in
  `rs/moq-net/src/lite/publisher.rs` and the JS wire cursor's `#reported` in
  `js/net/src/track.ts`. Delivery is recorded, not inferred from the cursor
  position: an arrival-order or descending cursor delivers out of sequence
  order, and #5929 re-offers below the position. The record only answers
  "re-offer or not"; the serving and re-offer path stays #5929's.
- **The record is bounded by the pool, never by guessing.** Each run is
  billed to the cache's `Pool` through a `Charge` at a fixed per-run cost
  and is settled a run at a time, so a cursor's
  state and CPU track its runs, never their span. The floor only rises over
  a final prefix: the runs from the floor up to the lowest run that is not
  final. Final means delivered, reported, or skipped as stale at the
  track's max age bound: only a widening reverses a verdict, and no
  widening reaches past the bound since every budget is clamped to it. A skipped stale run within reach is not final, and neither
  is an unknown hole: a descending or arrival-order cursor can deliver H
  while a lower G is still unknown, and folding H would silently treat G as
  handled. When the pool evicts one of a cursor's runs:
  - A run inside the final prefix folds into the floor, as `forget_below`
    does today, and nothing is lost.
  - Any other run cannot go without guessing, so the cursor fails: the
    subscription ends with `TooFarBehind` (stream code 0x5, already meaning
    a reader that fell too far behind and lost content), the subscriber
    re-subscribes, and the cursor's whole record is released with it.

  `js/net` has no byte pool and gets no new cap: a cursor's runs expire
  with the same idle window as the groups they sit between, under the same
  fold-or-`TooFarBehind` rule, so there are at most about one per cached
  sequence and the record grows no faster than the JS cache. The JS byte
  bound is [JS unbounded groups](/quest/m2/unbounded-groups/js.md).
- **Drops are final for now**, decided by the maintainer on 2026-10-19. A
  widening re-offers only groups this cursor never reported in
  SUBSCRIBE_DROP; a reported group stays dropped, as today. That leaves a
  known warm/cold gap: on lite-05 and lite-06 the publisher reports a stale
  skip as soon as it gives it up, so a group already reported is not
  re-offered, while a cold relay would serve it. Closing that gap needs
  per-cursor judgement of missing sequences, planned in
  [Drop set](/quest/m1/drop-set.md).
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
  subscriber count, cached groups, and runs per cursor record.
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
through the range miss fill, not a re-offer; and a stale group already reported in
SUBSCRIBE_DROP stays dropped after a widening, never served after its drop;
and a cursor that delivered F, skipped its neighbour G as stale while G
stayed cached, and delivered H, gets only G after a widening that covers
all three, through a bigger budget or a wider range, never F or H again;
and a run skipped at the max age bound folds into the floor; and a
cursor whose final prefix is evicted under pool pressure folds it and
resends nothing; and a descending cursor that delivered H while a lower G
is still unknown or skipped within reach, under pool pressure that evicts
H's run, ends with `TooFarBehind` instead of skipping G. Cover the
warm-relay case in `js/net` too.

Public API: none. Wire: no encoding change. A publisher now re-offers
cached groups it never reported dropped when a SUBSCRIBE_UPDATE widens the
max age or the range.

## Related

- [Drop set](/quest/m1/drop-set.md) - closes the gap this leaves for groups already reported dropped
- [JS unbounded groups](/quest/m2/unbounded-groups/js.md) - the JS byte bound for groups and the cursor runs between them
