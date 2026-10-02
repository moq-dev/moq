# Cluster routing

## Goal

Any MoQ node routes a subscription toward a broadcast's origin over whatever
links it holds, and a change in the network costs messages in proportion to
the origins it touches, not the broadcasts behind them. One protocol serves
every layout: moq.pro's edge and core tiers, a customer's own cluster, and a
drone swarm where some drones have an uplink, the rest reach it over a lossy
radio mesh, and every drone proxies for its neighbours. A node holding both a
direct link and a CDN link knows which one reaches an origin, so it uses the
CDN only when no direct route exists. The CDN never exposes its backbone to
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
  of one epoch path.
- **Next-hop authority.** A node stores every neighbour's announces but treats
  as live only those from its current next hop toward the origin. Babel's next
  hops toward one origin form a tree, so an END is final and no cycle of
  neighbours can keep a dead broadcast alive, while losing a neighbour just
  moves the next hop and adopts the stored copies.
- **Every hop re-selects.** No origin pinning in SUBSCRIBE: a pin breaks
  subscription aggregation and has no good answer when stale. Nearest-origin
  selection over consistent metrics is still a shortest-path tree, so it is
  loop-free once converged, and a transient cycle collapses into one
  aggregated subscription until re-selection moves it. The reply names the
  serving node for the splice rule.
- **Policy per link, not roles.** A link marked upstream never receives
  routes learned on another upstream link, and a route learned upstream keeps
  a down-only mark across other links so a mesh with two uplinks cannot leak
  CDN routes back into the CDN. An edge marks its core links upstream; a
  Starlink drone marks its CDN link upstream. See
  [Upstream links](/quest/m1/cluster-routing/transit.md).
- **One node id space, no cluster ids.** A customer cluster is nodes behind
  upstream or trusted links. Loop detection by cluster id would drop a
  partitioned swarm's traffic to itself through the CDN (BGP's partitioned-AS
  problem). Folding a large customer cluster into one id at its boundary is
  [Routing cost domains](/quest/m3/routing-cost-domains.md)'s.
- **Every session speaks it**, relays, apps, and browsers, in the wip lite
  version; a plain client with one link advertises only itself. Node ids are
  opaque randoms, so routes reveal no backbone addresses.
- **Trust.** Cluster-peer links may advertise any node; a client link's node
  ids stay scoped to its session. See
  [Route trust](/quest/m1/cluster-routing/route-trust.md).
- **Metric on the wire, cost policy local.** One additive metric. Static
  configured costs stay the default (the CDN); a radio link may measure its
  own ([Link quality](/quest/m1/cluster-routing/link-quality.md)).
- **Topology now, wire later.** moq.pro's edge and core migration runs on
  today's lite-06 path vector with upstream links; the route layer lands in
  the wip version afterwards with no re-layout, once
  [the simulator](/quest/m1/cluster-routing/sim.md) has compared it.

Kept from 2026-09-30: core links are configured and may skip PoPs; Warm and
Cold collapse to one cost ([One route cost](/quest/m1/route-cost.md));
epoch-qualified paths are a source's identity
([Selection](/quest/m1/cluster-routing/selection.md)); cluster links are
moq-lite only ([moq-transport peers are plain clients](/quest/m1/ietf-cluster-off.md));
`--hop` removal and One route cost land on `dev` on their own. Children
changing the wip lite version count as additive and land on `main`.
Anything specific to moq.pro's deployment is planned in moq.pro.

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

- [Upstream links](/quest/m1/cluster-routing/transit.md) - a link marked upstream never receives routes learned on another upstream link, which builds edge tiers and drone uplinks from one rule
- [Selection](/quest/m1/cluster-routing/selection.md) - a broadcast under overlapping prefixes routes to one origin deterministically, and same-epoch origins are one source
- [Simulate the split](/quest/m1/cluster-routing/sim.md) - moq.pro's simulator compares the route layer with path vector before the wire is written
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - ROUTE per origin node and path-less ANNOUNCE on one stream, loop-free by Babel feasibility
- [Route trust](/quest/m1/cluster-routing/route-trust.md) - a peer grant lets a client link advertise the nodes behind it as one identity
- [Link quality](/quest/m1/cluster-routing/link-quality.md) - a radio link's cost follows its measured quality without flapping routes

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - the longest-prefix rule, pool spread, and reply identity Selection builds on
- [P2P](/quest/m2/p2p/README.md) - browser and native peers that become routing nodes over this layer
- [Remove `--hop`](/quest/m1/hop-removal.md) - on `dev`: redundant publishers share an explicit `@<epoch>`
- [One route cost](/quest/m1/route-cost.md) - on `dev`: Warm and Cold collapse to one static cost
- [Same-hop importers](/quest/m1/hop-aligned-import.md) - the importer half of a redundant pair; `--hop` removal re-keys it to a shared epoch
- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - a redundant pair shares one epoch
- [Cross-relay delivery under bursts](/quest/m1/cross-relay-bursts.md) - its #4349 report shows closed broadcasts announced for up to 229 s
- [Routing cost domains](/quest/m3/routing-cost-domains.md) - policy and aggregation at boundaries between operators
