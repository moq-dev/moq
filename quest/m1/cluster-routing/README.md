# Cluster routing

## Goal

Any MoQ node routes a subscription toward a broadcast's origin over whatever
links it holds, and a change in the network costs messages in proportion to
the origins it touches, not the broadcasts behind them. One protocol serves
every layout: moq.pro's edge and core tiers, a customer's own cluster, and a
drone swarm where some drones have an uplink, the rest reach it over a lossy
radio mesh, and every drone proxies for its neighbours. A node holding both a
direct link and a CDN link knows which one reaches an origin, so it uses the
CDN only when no direct route exists, and an endpoint linked to several CDNs
prefers its primary and fails over to the next. The CDN never exposes its backbone to
end users, and a broadcast under overlapping prefixes routes to one origin
deterministically.

Non-goals: link-state topology, cache-aware route switching, TURN (the CDN
route is the fallback), and a permanently mixed-version cluster.

## Plan

### Decisions

Settled in the 2026-10-01 audit (#4694), replacing the per-broadcast path
vector the 2026-09-30 cache-tiers audit kept:

- **Two layers on one stream.** A ROUTE advertises reachability of one origin
  node (node id, seqno, metric); an ANNOUNCE says a prefix lives at a route's
  node and carries no path. Both ride the session's announce stream, so a
  ROUTE always precedes the ANNOUNCEs naming it. A link flap updates one ROUTE
  per affected origin instead of re-announcing every broadcast through it,
  and ending a broadcast is one fact with nothing to path-hunt. The hop list
  and the per-broadcast seqno plan (#4650) are gone. See
  [Routes and announces](/quest/m1/cluster-routing/routes.md).
- **Loop freedom per node, not per prefix.** Routes use Babel's feasibility
  condition (RFC 8966) keyed by node id. Babel was set aside earlier because
  it is not loop-free for a prefix with several origins (RFC 8966 s2.7);
  routing to a node removes that case, and a redundant pair is two ANNOUNCEs
  of one path under one epoch.
- **Next-hop authority.** A node stores every neighbour's announces but treats
  as live only those from its current next hop toward the origin. Babel's next
  hops toward one origin form a tree, so an END is final and no cycle of
  neighbours can keep a dead broadcast alive, while losing a neighbour just
  moves the next hop and adopts the stored copies.
- **Every hop re-selects.** No origin pinning in SUBSCRIBE: a pin breaks
  subscription aggregation and has no good answer when stale. Nearest-origin
  selection over consistent metrics is still a shortest-path tree, so it is
  loop-free once converged, and a transient cycle collapses into one
  aggregated subscription until re-selection moves it.
- **Policy per link, not roles.** A link marked upstream never receives
  routes learned on another upstream link, and a route learned upstream keeps
  a down-only mark across other links so a mesh with two uplinks cannot leak
  CDN routes back into the CDN. An edge marks its core links upstream; a
  Starlink drone marks its CDN link upstream. The per-link mark landed on
  today's path vector (`doc/bin/relay/cluster.md`); the down-only mark lands
  with [Routes and announces](/quest/m1/cluster-routing/routes.md).
- **One node id space, no cluster ids.** A customer cluster is nodes behind
  upstream or trusted links. Loop detection by cluster id would drop a
  partitioned swarm's traffic to itself through the CDN (BGP's partitioned-AS
  problem). Folding a large customer cluster into one id at its boundary is
  [Routing cost domains](/quest/m3/routing-cost-domains.md)'s.
- **Every session speaks it**, relays, apps, and browsers, in lite-07 (the
  current wip version, decided 2026-10-05); a plain client with one link advertises only itself. Node ids are
  opaque randoms, so routes reveal no backbone addresses.
- **Trust.** Cluster-peer links may advertise any node; a client link's node
  ids stay scoped to its session. Promoting them to a shared identity was
  dropped in the 2026-10-06 audit: session scoping already covers the
  security goal, and promotion has no consumer.
- **Multi-CDN by link preference.** A node may rank its links (moq.pro
  primary, Cloudflare secondary) ahead of the metric, so metrics from
  different operators are never compared; a CDN that speaks no ROUTE is just
  a link. See [Multi-CDN endpoints](/quest/m1/cluster-routing/multi-cdn.md).
- **Metric on the wire, cost policy local.** One additive metric on ROUTE,
  where each hop adds its link cost plus one so it strictly increases; the
  origin's per-prefix cost rides ANNOUNCE untouched (`transcode/**` at 10 and
  `transcode/foobar` at 1 from one node). Static
  configured costs stay the default (the CDN); a radio link may measure its
  own ([Link quality](/quest/m2/link-quality.md)).
- **Topology now, wire later.** moq.pro's edge and core migration runs on
  today's lite-06 path vector with upstream links; the route layer lands in
  lite-07 (the current wip version) afterwards with no re-layout, once
  [the simulator](/quest/m1/cluster-routing/sim.md) has compared it. Decided
  2026-10-05: this line gates
  [Finalize moq-lite-07](/quest/m1/lite07-finalize.md), and lite-07 loses
  its hop list and `Hop Base`/`Hop Keep` compression.

Kept from 2026-09-30: core links are configured and may skip PoPs; Warm and
Cold collapse to one cost ([One route cost](/quest/m1/route-cost.md));
One route cost lands on its own (`--hop` was replaced by `--epoch` in #4969). Decided 2026-10-06: a
path plus its [publisher epoch](/doc/concept/moq-lite.md#publisher-epochs) is
a source's identity, whoever serves it.
Anything specific to moq.pro's deployment is planned in moq.pro.
Decided 2026-10-08: [Link quality](/quest/m2/link-quality.md)
is Related, not Required, and moves to m2; measured cost is opt-in and the
line ships on static costs.

### Simulator findings

Measured on the earlier flat mesh with moq.pro's `just rs sim` (live's graph)
and `just rs sim sweep` (synthetic regional graphs of 34, 340, and 1020
relays).

- Existence costs about one message per relay per event flooded. On live, a
  wide customer prefix with `.stats` from every relay cost 2.5M announces
  under path vector and 4k flooded.
- Link-state liveness flooding dominates at scale: at 1020 relays (mean
  degree 35), a link loss and restore plus a relay loss and restart cost
  about 290 MiB of liveness against under 1 MiB of existence for twenty
  publishes. Per-node distance vector sends a route change only where the
  best route changes, which is what the split candidate must show.
- Failure detection, not routing, sets every outage window: a silent link or
  relay loss is noticed only after the QUIC idle timeout.
- HRW split an equal-cost pool 63/49 where a hash of the announced prefix
  sent all of it to one sibling.
- [#4399](https://github.com/moq-dev/moq/pull/4399) trades path hunting for
  flapping: each re-announce by a peer revives every route through it, stale
  ones included, and without coalescing the revivals cascade (18.6M messages
  instead of 189k). ANNOUNCEs that carry no path and follow the next hop end
  both.

### Remaining work

Once every child has landed:

- End-to-end tests in `rs/moq-relay/tests` with time mocked: a two-region
  tiered cluster (publish, end, core loss, redundant-pair failover reaching a
  far edge), and a drone mesh with two uplinks that partitions and heals
  (a broadcast from a drone without an uplink reaches a CDN subscriber, and
  each island reaches the other through the CDN).
- Rewrite `doc/bin/relay/cluster.md` into the operator's view (upstream
  links, tiers, dial rules, TLS edge links, link costs, redundant pairs), and
  add a routing page under `doc/concept` that walks both the CDN tiers and the
  drone mesh through the same ROUTE and ANNOUNCE rules.

## Required

- [Multi-CDN endpoints](/quest/m1/cluster-routing/multi-cdn.md) - an endpoint holds sessions to several CDNs, uses its preferred one, and fails over to the next
- [Simulate the split](/quest/m1/cluster-routing/sim.md) - moq.pro's simulator compares the route layer with path vector before the wire is written
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - ROUTE per origin node and path-less ANNOUNCE on one stream, loop-free by Babel feasibility

## Related

- [Link quality](/quest/m2/link-quality.md) - a radio link's cost follows its measured quality without flapping routes; not a blocker, since static costs are the default
- [P2P](/quest/m3/p2p/README.md) - browser and native peers that become routing nodes over this layer
- [One route cost](/quest/m1/route-cost.md) - Warm and Cold collapse to one static cost
- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - the importer half of a redundant pair under one explicit epoch
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - a redundant pair shares one epoch
- [Cross-relay bursts re-run](/quest/m1/cross-relay-bursts.md) - its #4349 report shows closed broadcasts announced for up to 229 s
- [Routing cost domains](/quest/m3/routing-cost-domains.md) - policy and aggregation at boundaries between operators
