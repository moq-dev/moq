# [XL] Routes and announces

## Goal

Routing info splits into two layers on the session's announce stream, in
lite-07 (the current wip version, decided 2026-10-05), for every session. A ROUTE advertises reachability of one
origin node; an ANNOUNCE says a prefix lives at a route's node and carries no
path. A link flap or relay loss sends one ROUTE change per origin whose best
route changed, never a re-announce per broadcast; ending a broadcast reaches
each node about once and nothing hunts; failover to another neighbour with
the same epoch is seamless; and no state of disagreeing neighbours keeps a dead broadcast
alive.

## Plan

Decided 2026-10-01 (see the [line's decisions](/quest/m1/cluster-routing/README.md)):

- Wire sketch, flexible in detail. ROUTE_START carries the node id, a seqno,
  the metric, and a down-only bit, and implicitly takes the next stream-local
  Route ID, as ANNOUNCE_START takes the next Announce ID today. ROUTE_UPDATE
  (Route ID, seqno, metric, down-only bit) and ROUTE_END (Route ID) reference
  it; the bit rides UPDATE so a route moving between a mesh path and a CDN
  fallback changes it without retracting its announces.
  ANNOUNCE_START carries the prefix (keeping today's Path Base/Keep
  compression), a Route ID, the origin's cost for that prefix, and the epoch,
  and lite-07's restart message from
  [Restart](/quest/m0/broadcast-epoch/restart.md) keeps its meaning. On a
  route without an epoch, the source is the origin node and its ANNOUNCE: a
  new one at the same path is a restart.
  ANNOUNCE_UPDATE re-prices it and ANNOUNCE_END ends one broadcast while the
  route stays up. An announce never changes its Route ID: another origin
  serving the same path is another ANNOUNCE. An ANNOUNCE naming an unknown
  Route ID is a protocol violation, and ROUTE_END ends that stream's
  ANNOUNCEs on the route. The hop list leaves lite-07, and with it the
  `Hop Base`/`Hop Keep` compression.

  ```text
  ROUTE_START  node=0x7a3f seqno=41 metric=12   -> route 0
  ANNOUNCE_START prefix=transcode/ route=0 cost=10    -> announce 0
  ANNOUNCE_START prefix=transcode/foobar route=0 cost=1 -> announce 1
  ROUTE_UPDATE route=0 seqno=42 metric=15
  ANNOUNCE_END announce=1                       # one broadcast ends
  ROUTE_END    route=0                          # ends announce 0 too
  ```
- A stream carries a ROUTE only while some ANNOUNCE on it references it: sent
  just before the first, ended after the last. That bounds a client session
  to the origins of what it asked for (announce interest already scopes it),
  not the CDN's whole node table.
- The announce cost is the origin's per-prefix seed (`transcode/**` at 10 and
  `transcode/foobar` at 1 from one node, decided 2026-10-01) and nobody
  changes it in transit; a path's cost through a route is that seed plus the
  route metric. Link changes therefore touch only ROUTE, and feasibility runs
  on the route metric alone. This is today's single route cost, which stops
  accumulating per hop once this lands.
- Each hop adds its link cost plus one to the metric (decided 2026-10-01), so
  the metric strictly increases as Babel requires while operators keep
  configuring cost 0 for a free link.
- The node id is global and opaque: a relay's `cluster.id` or a random id,
  and an app's handshake id. It is needed so routes from two neighbours to
  one origin are recognized as one, which carries loop freedom,
  deduplication, and P2P dialing (an app maps an announce's node to a roster
  peer).
- Loop freedom is Babel's feasibility condition (RFC 8966) keyed by node:
  accept a route if its seqno is newer, or equal with a metric below the
  feasibility distance. Only the origin advances its seqno. Retraction is an
  infinite metric or ROUTE_END; there is no count to infinity.
- Next-hop authority: store every neighbour's ANNOUNCEs, but a prefix is live
  at a node only as its current next hop toward the origin announces it, the
  neighbour a subscribe for that path would go to (metric, then the path-keyed
  rendezvous hash among equal next hops). When the next hop changes, adopt
  the new neighbour's stored set and send downstream only the difference.
  No per-announce seqno.
- Origin selection keeps `route_order`'s shape and swaps its inputs: the
  candidates become the origin nodes announcing the path, ranked by longest
  prefix, then the newest epoch, then the link's preference
  ([Multi-CDN endpoints](/quest/m1/cluster-routing/multi-cdn.md)), then the
  route metric to the node, then the path-keyed rendezvous hash, with the
  origin node id breaking a full tie.
- Down-only bit: set on a route learned on an upstream link, kept across
  other links, and a route carrying it is never sent on an upstream link
  (extending the `upstream` link mark in `doc/bin/relay/cluster.md`).
- A plain client with one link advertises a ROUTE for itself and the
  ANNOUNCEs it publishes; its node id is scoped to its session, and
  nothing promotes it to an identity shared across sessions.
  A relay advertising routes to a client sends them as usual; node ids reveal
  nothing about the backbone.
- Every hop re-selects; SUBSCRIBE names no origin, and the reply names none
  either: any route announcing a path under the same epoch resumes it.
- Mixed versions: a lite-06 peer keeps today's path vector, translated at the
  relay that speaks both, for the rollout window only.

Why not a hold-down: #4644 retracted at once and re-announced a replacement
after 1 s, which ended a downstream front whose only path ran through the
relay, dropping subscribers for about a second while the relay kept serving;
holding without retracting brought hunting back (moq-dev/moq.pro#2116 has the
measurements, and #4642's 300 ms cursor hold is the production mitigation).
Separating "the path changed" (ROUTE) from "the broadcast ended" (ANNOUNCE)
removes that trade, and the failover tests must show no front loses its
route while a working one exists. Decide whether #4642's hold can then go.

Open, for the implementer to settle and record:

- Seqno lifetime across restarts of a node with a configured stable id
  (persist it, fold an incarnation into the id, or Babel's seqno request).
- Whether starvation recovery needs a request message, or session restart
  covers it.
- The window where an origin's ANNOUNCE_END and a next-hop change cross in
  flight, and whether it needs more than the next END to arrive.
- How much of today's route trie, fronts, and `route_order` survives intact.

Tests, time mocked: #4644's live-graph withdrawal and failover tests
(`quest/m0/path-hunting` branch) as acceptance, run on the wip version; a
link flap with many broadcasts behind one origin costs one ROUTE change per
link; a ring of three where a stale neighbour cannot revive an ended
broadcast; a two-uplink mesh that never carries CDN routes back to the CDN;
an island reached through the CDN after a partition; an unknown Route ID
refused.

Wire: `drafts/draft-lcurley-moq-lite.md` in the same PR, and `js/net`
encodes, decodes, and resolves it (JS transit stays in
[P2P](/quest/m3/p2p/README.md)). Public API: the route-change surface on
`broadcast::Route` and its bindings will likely change; report it. This may
split at start (Rust and draft, then JS), as long as both land in one
release.

## Required

- [Simulate the split](/quest/m1/cluster-routing/sim.md) - the numbers that confirm the design before the wire is written

## Related

- [Restart](/quest/m0/broadcast-epoch/restart.md) - the lite-07 restart message ANNOUNCE_START sits beside
