# Tree-routed announcements

## Goal

A route change in a relay cluster costs announces in proportion to the relays
that need the route, not the links between them. Today every relay forwards
its best route to every neighbour but the one it came from, so one broadcast
start or stop sends about `2E - (N - 1)` announces and every relay holds one
copy per neighbour. On moq.pro's 34-relay, 140-link mesh that is ~247 per
event, and [PoP skipping](/quest/m1/pop-skipping/README.md) roughly doubles
`E`. After this line a relay hears each broadcast from at most two neighbours,
a parent and a backup that it chose itself. A relay whose backup avoids its
parent keeps the broadcast through that parent's failure with no round trip.
This holds on lite and IETF cluster sessions alike; customer sessions are
unchanged.

Non-goals: a relay still learns every route in the cluster (pull or scoped
interest was dropped in [relay memory](/quest/m1/relay-memory.md)), and bytes
per announce belong to the [prefix table](/quest/m2/announce-prefix-table.md).

## Plan

### Receivers choose

No relay needs the cluster graph. Every relay announces a beacon, and beacons
are routed like any broadcast. So each relay already holds its best route, by
its own `route_order`, to every other relay's beacon, plus the standby routes
its neighbours offered. From those, relay `P` derives for each source relay
`S`:

- the tie set `U_S`: the neighbours whose beacon routes to `S` tie for best;
- for each member `u` of `U_S`, a backup `b_u`: its best standby route to `S`
  whose chain avoids `u` (node-protecting). Failing that, the best route over
  a session other than `u`'s (link-protecting). Failing that, none.

`P` publishes that table. For a route with prefix `X` from source `S`, the
parent is the member of `U_S` that ranks highest by rendezvous hash of
`(X, member)`, and the backup is that member's `b`. A neighbour sends the
route to `P` only if it is that parent or that backup. The source is the first
hop in the route's chain that has a beacon.

The neighbour floods, exactly as today, in these cases:

- `P` publishes no table (a customer or an older relay);
- the source has no entry in `P`'s table (unreachable, still converging, or a
  chain with an anonymous hop).

So a split mesh, say two new relays joined only through an old one, floods
across the gap instead of dropping routes.

### Consistent ranking

A cursor carries one route per path, so completeness needs every relay to rank
competing sources for the same prefix the same way. For example, a warm carrier
and the origin.

[Rendezvous ranking](/quest/m1/announce-tree/route-order.md) makes the
tie-breaks consistent: cost and hop count add up per link, then ties rank by
rendezvous hash of `(prefix, source)`, then of `(prefix, next hop)`.

With additive keys and a tie-break that depends only on the source, if
`P` prefers `S` to `C`, so does `P`'s parent toward `S`. So each relay's
chosen upstream holds the route it expects, and a warm route stops at the edge
of its carrier's catchment.

### Failover and change

When `P`'s parent fails, `P` already holds the backup's route. It promotes it
and sends downstream an in-place update (lite-06 `ANNOUNCE_RESTART`, IETF
`PublishNamespaceUpdate`), then republishes its table.

A relay with only a link-protecting backup, or none, loses the route when its
parent relay dies, until the beacons reconverge.

Table changes take effect at once: the neighbours re-sync `P`'s cursors
against the new table.

## Quests

- [Announce counters](/quest/m1/announce-tree/counters.md) - relay `/metrics`
  exports per-tier announce starts, ends, updates, and encoded announce bytes
- [Rendezvous ranking](/quest/m1/announce-tree/route-order.md) - `route_order`
  breaks cost ties per prefix by rendezvous hash of the source, then of the
  next hop, identically at every relay
- [Beacons and upstream tables](/quest/m1/announce-tree/beacons.md) - every
  relay announces a beacon and publishes its parent tie set and backups per
  source, with forwarding unchanged
- [Cluster simulator](/quest/m1/announce-tree/simulator.md) - seeded random
  meshes, failures, and version mixes check route completeness and copy
  bounds after every step
- [Tree forwarding](/quest/m1/announce-tree/forward.md) - cluster peers send a
  route only to the receivers that chose them, flooding whatever a receiver
  has no choice for

## Related

- [Relay memory](/quest/m1/relay-memory.md) - route copies per relay fall from
  degree to at most two
- [PoP skipping](/quest/m1/pop-skipping/README.md) - its skip links and warm
  routes need no special case
- [Prefix table](/quest/m2/announce-prefix-table.md) - fewer bytes per
  announce, orthogonal to fewer announces
- [Stats linger](/quest/m1/stats-linger.md) - removes a churn source rather
  than its fanout
