# [L] lite-07 route wire

## Goal

lite-07 carries the route layer of [Routes and announces](/quest/m1/cluster-routing/routes/README.md) on
the announce stream: ROUTE_START, ROUTE_UPDATE, ROUTE_END, and
ANNOUNCE_FLOOR toward the subscriber, and ROUTE_REQUEST and the ROUTE_END
echo back toward the publisher. ANNOUNCE_START and ANNOUNCE_UPDATE carry a
Route ID and the origin's seed in place of the hop list. Rust encodes and
decodes it, and `drafts/draft-lcurley-moq-lite.md` specifies it. Driving the
router with it is [session integration](/quest/m1/cluster-routing/routes/session.md).

## Plan

- Wire sketch, flexible in detail. ROUTE_START carries the node id, a seqno,
  the metric, and the down-only bit (the epic's one-selection decision), and implicitly takes the next
  stream-local Route ID, as ANNOUNCE_START takes the next Announce ID today.
  ROUTE_UPDATE (Route ID, seqno, metric, down-only) and ROUTE_END (Route
  ID) reference it; the bit rides UPDATE so a route moving between a
  mesh path and a CDN fallback changes it without retracting its announces.
  ANNOUNCE_START carries the prefix (keeping today's Path Base/Keep
  compression), a Route ID, the origin's cost for that prefix, and the epoch,
  and lite-07's restart message from
  Restart keeps its meaning. On a
  route without an epoch, the source is the origin node and its ANNOUNCE: a
  new one at the same path is a restart. This is routing provenance, not
  permission to stitch. A concrete serving-route change without an epoch
  remains Restart even when the origin node is unchanged; only a matching
  explicit epoch or the separately proven Takeover operation preserves an
  instance (superseded 2026-10-30: a shared route `instance` preserves it, per
  [Instance and epoch](https://github.com/moq-dev/moq/pull/7489)). Test a same-origin next-hop change with no epoch.
  ANNOUNCE_UPDATE re-prices it and ANNOUNCE_END withdraws this origin's claim
  while the route stays up. A wildcard claim is capability, not an inventory
  of broadcasts; another origin may still serve the same epoch. An announce
  never changes its Route ID: another origin
  serving the same path is another ANNOUNCE. An ANNOUNCE naming an unknown
  Route ID is a protocol violation. A ROUTE_UPDATE with an infinite metric
  (2^64-1, the largest lite-07 varint; a sum reaching it is unreachable)
  means held: the sender has no usable route but still holds the origin's
  announcements while it recovers. ROUTE_END means the sender let go of the
  origin on this stream; neither is proof that the origin withdrew its
  claims (see the epic's handoff contract). Each batch of ANNOUNCEs for a route
  closes with one ANNOUNCE_FLOOR listing every region the sender certifies
  for that route: a count, then per region a suffix of the stream's interest
  prefix (empty for the whole interest) and the origin revision as of which
  the stream's set for that route in that region is complete. The list
  replaces the previous one, so a region left out is no longer certified,
  and a count of zero answers a stream whose interest none of the sender's
  regions meets.
  ROUTE_REQUEST (node id, seqno) travels
  the other way on the same stream, toward the neighbour that sent the route,
  and so does the echo of every ROUTE_END: the receiver sends the same
  ROUTE_END (Route ID) back once it has dropped the route, which tells the
  sender no neighbour still holds its advertisement (see the epic's
  handoff contract). Every receiver echoes, clients included; a closed session
  counts as the echo.
  The hop list leaves lite-07, and with it the
  `Hop Base`/`Hop Keep` compression; `cluster.max_hops` then stops applying
  to lite-07. Most of the 62% saving measured in
  [moq-dev/moq#4196](https://github.com/moq-dev/moq/pull/4196) came from hop
  chains, so rerun its announce bench on path-less ANNOUNCE and drop
  `Path Base`/`Path Keep` too unless it still pays for its rules.

  ```text
  ROUTE_START  node=0x7a3f seqno=41 metric=12   -> route 0
  ANNOUNCE_START prefix=transcode/ route=0 cost=10    -> announce 0
  ANNOUNCE_START prefix=transcode/foobar route=0 cost=1 -> announce 1
  ANNOUNCE_FLOOR route=0 count=1 ""@7           # complete as of revision 7
  ROUTE_UPDATE route=0 seqno=42 metric=15
  ANNOUNCE_END announce=1                       # this origin withdraws a claim
  ANNOUNCE_FLOOR route=0 count=1 ""@8
  ANNOUNCE_FLOOR route=1 count=2 "live/a/"@3 "live/b/"@5  # a scoped key
  ANNOUNCE_FLOOR route=1 count=1 "live/b/"@6    # live/a/ is no longer certified
  ROUTE_UPDATE route=0 seqno=42 metric=inf      # held: unusable, still recovering
  ROUTE_END    route=0                          # let go: forget it on this stream
  ROUTE_END    route=0                          # <- the receiver echoes it back
  ```
- Requests forwarded between routing nodes name the serving identity
  (decided 2026-10-13). ROUTE carries one down-only bit and no export metric.
- The region list's order and any other wire detail of it are settled here
  with Rust and JS interop, not in the proof. The core keeps regions sorted
  by path.
- A hop-less ANNOUNCE_START cannot run before the router drives sessions, so
  the new messages may land first and the ANNOUNCE layout switch with
  [session integration](/quest/m1/cluster-routing/routes/session.md), as long as every lite-07 build
  between them still interoperates with itself.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes/README.md) - the contract the wire carries
- [A job's exact route continues its claim's readers](/quest/m1/announce-takeover.md) - also edits lite-07's ANNOUNCE messages
