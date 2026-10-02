# [XL] Routes and announces

## Goal

Routing info splits into two layers on the session's announce stream, in the
wip lite version, for every session. A ROUTE advertises reachability of one
origin node; an ANNOUNCE says a prefix lives at a route's node and carries no
path. A link flap or relay loss sends one ROUTE change per origin whose best
route changed, never a re-announce per broadcast; ending a broadcast reaches
each node about once and nothing hunts; failover to another neighbour is
seamless; and no state of disagreeing neighbours keeps a dead broadcast
alive.

## Plan

Decided 2026-10-01 (see the [line's decisions](/quest/m1/cluster-routing/README.md)):

- Wire sketch, flexible in detail. ROUTE_START carries the node id, a seqno,
  the metric, and a down-only bit, and implicitly takes the next stream-local
  Route ID, as ANNOUNCE_START takes the next Announce ID today. ROUTE_UPDATE
  (Route ID, seqno, metric) and ROUTE_END (Route ID) reference it.
  ANNOUNCE_START carries the prefix (keeping today's Path Base/Keep
  compression) and a Route ID. An ANNOUNCE naming an unknown Route ID is a
  protocol violation, and ROUTE_END ends that stream's ANNOUNCEs on the
  route. The hop list and per-announce cost leave this version.

  ```text
  ROUTE_START  node=0x7a3f seqno=41 metric=12   -> route 0
  ANNOUNCE_START prefix=drone/cam1 route=0      -> announce 0
  ROUTE_UPDATE route=0 seqno=42 metric=15
  ROUTE_END    route=0                          # ends announce 0 too
  ```
- The node id is global and opaque: a relay's `cluster.id` or a random id,
  and an app's handshake id. It is needed so routes from two neighbours to
  one origin are recognized as one, which carries loop freedom,
  deduplication, the reply's serving node, and P2P dialing (an app maps an
  announce's node to a roster peer).
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
- Down-only bit: set on a route learned on an upstream link, kept across
  other links, and a route carrying it is never sent on an upstream link
  ([Upstream links](/quest/m1/cluster-routing/transit.md)).
- A plain client with one link advertises a ROUTE for itself and the
  ANNOUNCEs it publishes; its node id is scoped to its session (shared
  identity across sessions is [Route trust](/quest/m1/cluster-routing/route-trust.md)).
  A relay advertising routes to a client sends them as usual; node ids reveal
  nothing about the backbone.
- Every hop re-selects; SUBSCRIBE names no origin. The reply names the
  serving node, which is the identity Selection's splice rule uses.
- Mixed versions: a lite-06 peer keeps today's path vector, translated at the
  relay that speaks both, for the rollout window only.

Open, for the implementer to settle and record:

- Seqno lifetime across restarts of a node with a configured stable id
  (persist it, fold an incarnation into the id, or Babel's seqno request).
- Whether starvation recovery needs a request message, or session restart
  covers it.
- The window where an origin's END and a next-hop change cross in flight,
  and whether it needs more than the next END to arrive.
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
[P2P](/quest/m2/p2p/README.md)). Public API: the route-change surface on
`broadcast::Route` and its bindings will likely change; report it. This may
split at start (Rust and draft, then JS), as long as both land in one
release.

## Required

- [Simulate the split](/quest/m1/cluster-routing/sim.md) - the numbers that confirm the design before the wire is written
