# Tree-routed announcements

## Goal

A route change in a relay cluster costs announces in proportion to the relays
that need the route, not the links between them. Today every relay forwards
its selected route to every neighbour but the one it came from, so one
broadcast start or stop sends about `2E - (N - 1)` announces and every relay
holds one copy per neighbour. On moq.pro's 34-relay, 140-link mesh that is
~247 per event, and [PoP skipping](/quest/m1/pop-skipping/README.md) roughly
doubles `E`. After this line a relay hears each route from at most two
neighbours: its parent in the shortest-path tree rooted at the route's source,
and one backup that avoids that parent. A failed link or relay anywhere on a
path still fails over locally, with no round trip. The announce wire and
customer sessions are unchanged.

Non-goals: a relay still learns every route in the cluster (pull or scoped
interest was dropped in [relay memory](/quest/m1/relay-memory.md)), and the
bytes per announce belong to the [prefix table](/quest/m2/announce-prefix-table.md).

## Plan

### The rule

A relay forwards only its selected route for a path, as today, and on a
cluster peer session only to:

- the peers it is the parent of in the shortest-path tree rooted at the
  route's source, the first hop in its chain that is a node of the graph;
- the peer it is the backup for: for peer `P` whose parent is `Q`, the
  cheapest neighbour of `P` other than `Q` whose own tree path to the source
  avoids both `P` and `Q`. If none does, the cheapest neighbour whose path
  avoids `P`, which protects the `P`-`Q` link but not `Q` itself. If none
  does either, `P` has no backup.

Every relay computes the same trees from the same graph: link prices are the
configured cluster costs, and ties break on the lower hop id. A route whose
chain has no graph node is flooded as today.

The rule has no special case for warm routes. A carrier's exact-path route
from [warm advertise](/quest/m1/pop-skipping/warm-advertise.md) spreads down
its carrier's tree only while it is each relay's selected route. Under
additive prices, if a relay prefers source `S` over carrier `C`, every relay
whose shortest path to `C` passes through it prefers `S` too. So a route stops
at the edge of its source's catchment, and each relay hears its nearest source
plus a backup.

### Failover

A relay with a node-protecting backup survives its parent failing: the relay
next to a failed link or relay promotes its backup and sends downstream an in-place
update (lite-06 `ANNOUNCE_RESTART`), not an end, so downstream relays never
reconverge. A relay with only a link-protecting backup, or none, loses the
route when its parent relay fails, and reconverges when the topology digest
changes. That keeps every relay at two copies at most on any graph. The
topology quest reports how many relays lack a node-protecting backup, so a
graph that needs one more link shows it.

### Agreement

Each relay publishes its live links, and the graph is their union. A link
counts only when both ends list it. A relay forwards in tree mode to a peer
only while both publish the same graph digest; otherwise it floods that link,
as today. An older relay publishes no digest, so it is always flooded. While
the topology changes, the links in disagreement carry a superset. Tree mode
resumes by ending the routes a peer no longer needs, so no relay is left
without a route.

## Quests

- [Announce counters](/quest/m1/announce-tree/counters.md) - relay `/metrics`
  exports per-tier announce starts, ends, updates, and encoded announce bytes
- [Cluster topology](/quest/m1/announce-tree/topology.md) - every relay
  publishes its live links and the cluster settles on one graph and digest,
  with forwarding unchanged
- [Tree forwarding](/quest/m1/announce-tree/forward.md) - cluster peers
  forward along the source's tree plus a backup, and flood any link whose ends
  disagree on the graph

## Related

- [Relay memory](/quest/m1/relay-memory.md) - route copies per relay fall from
  degree to at most two
- [PoP skipping](/quest/m1/pop-skipping/README.md) - its priced skip links are
  more edges in the same graph
- [Prefix table](/quest/m2/announce-prefix-table.md) - fewer bytes per
  announce, orthogonal to fewer announces
- [Stats linger](/quest/m1/stats-linger.md) - removes a churn source rather
  than its fanout
