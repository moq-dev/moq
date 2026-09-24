# [L] Tree forwarding

## Goal

On lite-07 cluster peer sessions, a relay sends a route only to
the receivers that chose it as parent or backup for that route's prefix and
source, following the [line's rule](/quest/m1/announce-tree/README.md). It
floods whatever a receiver has no choice for. On a sparse mesh where every relay speaks
lite-07, a broadcast start costs at most `2(N - 1)` announce starts in steady state instead of
`2E - (N - 1)`. A relay whose backup is node-protecting keeps its routes
through its parent's failure with no gap. Customer sessions and the wire are
unchanged, so this targets `main`.

## Plan

- Apply the peer's latest UPSTREAMS table, from its
  [cluster stream](/quest/m1/announce-tree/cluster-stream.md), in the shared
  cursor layer in `rs/moq-net/src/model/origin.rs`. A cursor whose session
  has a table is in tree mode. Any other cursor, including customers, lite-06
  peers, and IETF links, sees today's behaviour.
- A route's source for this rule is the first hop in its chain that is a
  known relay in [reachability](/quest/m1/announce-tree/reachability.md).
- Split horizon filters before selection today, so a peer receives this
  relay's best route that doesn't come through it. Keep that selection and
  apply the tree check after it. If the check fails, the cursor presents
  nothing for the prefix; it never falls through to a lower-ranked route.
  Falling through would leak alternates to every neighbour.
- Parent and backup are computed per prefix from the receiver's tie set, by
  the same rendezvous hash as
  [ranking](/quest/m1/announce-tree/route-order.md).
- A source with no entry in the receiver's table is flooded. This covers
  unreachable sources, anonymous chains, and meshes split by an older relay.
- When a table update arrives, re-sync that peer's cursors. Today
  `sync_cursor` runs only on a route change, so add that path. It starts
  routes where this relay newly qualifies and ends them where it no
  longer does.
- With several sessions to one peer hop, send on one of them only.
- A relay flag, on by default, controls whether the relay sends its upstream
  table. Turning it off makes every neighbour flood to that relay
  again, so one relay can be rolled back with a config change.

The [simulator](/quest/m1/announce-tree/simulator.md) must hold completeness,
the copy bound, failover, and loop freedom across a sweep of seeds. Add named
regression cases for:

- the split mesh through an older relay;
- competing sources at a tie;
- an IETF cluster link and a lite-06 cluster link, which both flood.

Add a benchmark beside `origin/announce_duplicate` sweeping route count
against cluster-peer count, with tree forwarding on and off, so a cost that
grows per route and peer, not per touched path, shows up as a slope. Re-syncing
on a table change is the obvious candidate.

## Required

- [Relay reachability and upstream tables](/quest/m1/announce-tree/reachability.md) -
  the choices the rule reads
- [Cluster simulator](/quest/m1/announce-tree/simulator.md) - the invariant
  check it must pass
