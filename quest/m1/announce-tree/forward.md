# [L] Tree forwarding

## Goal

Cluster peer sessions forward routes by the
[line's rule](/quest/m1/announce-tree/README.md): each relay's selected route
goes to the peers it is the tree parent of and the peers it is the backup for.
On a sparse mesh, a broadcast start costs at most `2(N - 1)` announce starts
instead of `2E - (N - 1)`. Losing any single link or relay on a path leaves
downstream subscribers playing with no group gap. Customer sessions and the
wire are unchanged, so this targets `main`.

## Plan

- Per graph digest, precompute for each (source, peer) whether this relay is
  the peer's parent or backup: `N` Dijkstra runs over a graph of tens of
  nodes, cached until the digest changes.
- The lite publisher consults that table when handing a route to a cluster
  peer. It checks only when both ends publish the same digest and the route's
  source is in the graph; otherwise it floods as today.
- When the selected route's source changes, or the digest does, re-evaluate
  the peers it goes to: start where the relay newly qualifies, end where it no
  longer does.
- A peer whose digest differs is flooded until it matches, so disagreement
  sends a superset and never leaves a relay without a route.

Tests, in-process, driven by the test clock with no sleeps:

- On an eight-relay ring with chords, the
  [counters](/quest/m1/announce-tree/counters.md) show at most `2(N - 1)`
  starts per broadcast start, and the flooding count on the same mesh.
- Killing a mid-path relay, and separately cutting a link, leaves a
  subscriber three hops away with no group gap; downstream relays see only
  in-place updates.
- A mesh with one relay in flood mode (no digest) still delivers to every
  relay.
- Through a topology change, every relay holds the route at every step.
- A warm exact-path route from a second carrier reaches only the relays
  nearer that carrier.

## Required

- [Cluster topology](/quest/m1/announce-tree/topology.md) - the graph and
  digest the rule reads
