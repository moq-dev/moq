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
  Restart keeps its meaning. On a
  route without an epoch, the source is the origin node and its ANNOUNCE: a
  new one at the same path is a restart. This is routing provenance, not
  permission to stitch. A concrete serving-route change without an epoch
  remains Restart even when the origin node is unchanged; only a matching
  explicit epoch or the separately proven Takeover operation preserves an
  instance. Test a same-origin next-hop change with no epoch.
  ANNOUNCE_UPDATE re-prices it and ANNOUNCE_END ends one broadcast while the
  route stays up. An announce never changes its Route ID: another origin
  serving the same path is another ANNOUNCE. An ANNOUNCE naming an unknown
  Route ID is a protocol violation, and ROUTE_END ends that stream's
  ANNOUNCEs on the route. ROUTE_REQUEST (node id, seqno, hop count) travels
  the other way on the same stream, toward the neighbour that sent the route.
  The hop list leaves lite-07, and with it the
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
- Starvation recovery is Babel's seqno request (RFC 8966 s3.8, decided
  2026-10-12). A node whose routes to an origin are all infeasible sends
  ROUTE_REQUEST for that origin, with its feasibility seqno plus one, to the
  neighbour it would use. A node holding a route at least that new answers
  with it, and only the origin advances its seqno to answer. Otherwise, while
  the hop count is at least two, it forwards the request with the hop count
  decremented, never to the requesting neighbour (RFC 8966 s3.8.1.2): through
  a feasible route's next hop if one avoids the requester, else through an
  infeasible route's, else it drops the request. A node keeps one request
  per origin and seqno pending, suppressing duplicates, and resends its own
  a small number of times; the pending entry expires when answered, when the
  route is gone, or when the retries run out, and only a later infeasible
  update starts a new one. Feasibility is never
  reset to accept a longer route after a retraction or a session replay.
- Next-hop authority: store every neighbour's ANNOUNCEs, but a prefix is live
  at a node only as its current next hop toward the origin announces it, the
  neighbour a subscribe for that path would go to (metric, then the path-keyed
  rendezvous hash among equal next hops). When the next hop changes, adopt
  the new neighbour's stored set and send downstream only the difference.
  No per-announce seqno.
- Origin selection keeps `route_order`'s shape and swaps its inputs: the
  candidates become the origin nodes announcing the path, ranked by longest
  prefix, then the newest epoch, then the link's preference
  (`origin::Producer::with_preference`), then the
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
holding without retracting brought hunting back in measurements, and
#4642's 300 ms cursor hold is the production mitigation.
Separating "the path changed" (ROUTE) from "the broadcast ended" (ANNOUNCE)
removes that trade, and the failover tests must show no front loses its
route while a working one exists. Decide whether #4642's hold can then go.

Simulator findings (2026-10-12), measured with seed 1 on a tiered
45-node topology (four regions of two upstream nodes and their edges, plus
five single-node regions), tiered synthetic graphs of 347 and 1024 nodes,
and a six-node radio mesh with two uplinks. Bytes are uncompressed estimates
with eight-byte ids; these are protocol metrics in virtual time, not runtime
benchmarks.

- Starvation: a five-node ring loses its preferred origin link while a
  working four-hop backup remains. The backup's same-seqno route stays
  infeasible for good, and restarting the backup's session does not help;
  only an origin seqno advance does. Hence the seqno request above.
- Ending a broadcast on the 45-node topology costs 124.8 KiB against path
  vector's 2817.0 KiB (1304.0 KiB with #4399's withdrawal hiding), but still
  2000 client updates against a 900 once-only minimum (up to 11 per node
  and path), with 550 re-announcements. The synthetic graphs reach about
  three times the minimum (20640 against 6940, 62182 against 20480).
  Next-hop authority alone does not meet the Goal's "reaches each node
  about once".
- Losing one of an edge's two upstream links leaves live broadcasts reported
  as gone for 722.8 aggregate seconds before restoration, where path vector
  reports none: the route moves, but the announces behind it trail.
- The down-only bit holds: with an uplink edge cut off from both of its
  upstream nodes but still linked to the mesh, path vector routes its
  subscriptions through the radio mesh and the split refuses, and after a
  partition each island reaches the other through its uplink.

Open, for the implementer to settle and record:

- Seqno lifetime across restarts of a node with a configured stable id
  (persist it, fold an incarnation into the id, or rely on the seqno
  request). The simulator persisted each origin's counter across restarts to
  keep this choice from hiding starvation.
- The window where an origin's ANNOUNCE_END and a next-hop change cross in
  flight, and what makes a replacement next hop's announce set current. Kept
  open: [Next-hop handoff](/quest/m1/cluster-routing/handoff.md) settles it
  with deterministic acceptance tests before the wire is written.
- How much of today's route trie, fronts, and `route_order` survives intact.

Tests, time mocked: #4644's live-graph withdrawal and failover tests
(`quest/m0/path-hunting` branch) as acceptance, run on the wip version; a
link flap with many broadcasts behind one origin costs one ROUTE change per
link; a ring of three where a stale neighbour cannot revive an ended
broadcast; a two-uplink mesh that never carries CDN routes back to the CDN;
an island reached through the CDN after a partition; an unknown Route ID
refused; a five-node ring whose preferred origin link fails recovers its
longer backup through a seqno request, duplicates suppressed, while that
backup never loses its session; a ring where a node's own preferred route
points back at the requester forwards the request around it instead, and an
unanswerable request stops after its retries. Failover is measured
on broadcast liveness while the next hop changes, including a new next hop
whose ROUTE arrives before its ANNOUNCE set, and reports per-path
retractions and re-announcements beside the ROUTE budget; eventual route
recovery alone does not show it is seamless.

Wire: `drafts/draft-lcurley-moq-lite.md` in the same PR, and `js/net`
encodes, decodes, and resolves it (JS transit stays in
[P2P](/quest/m3/p2p/README.md)). Public API: the route-change surface on
`broadcast::Route` and its bindings will likely change; report it. This may
split at start (Rust and draft, then JS), as long as both land in one
release.

## Required

- [Next-hop handoff](/quest/m1/cluster-routing/handoff.md) - what keeps an END final and a replacement next hop's announces current across the handoff

