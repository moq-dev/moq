# [M] Cluster topology

## Goal

Every cluster relay publishes its live peer links, and once the cluster
settles, every relay holds the same graph and the same digest of it.
Forwarding is unchanged. The relay's internal HTTP listener reports the digest,
node count, link count, and the relays with no node-protecting backup, so an
operator can see agreement and the graph's weak spots.

## Plan

- Each relay announces `.internal/topology/<node>` carrying its hop id and, for
  every live cluster peer session, the peer's hop id and the link price
  (`?cost=`, or 1). It republishes when a peer session opens or closes and
  retracts on shutdown.
- `.internal/topology/` routes are always flooded, so the graph bootstraps
  regardless of tree mode.
- The graph is the union of the entries. A link counts only when both ends
  list it, so a half-open session or an older relay that publishes nothing
  drops out.
- The digest is a hash of the canonical sorted link list. Each relay publishes
  the digest it currently holds on a second track of its topology broadcast,
  which [tree forwarding](/quest/m1/announce-tree/forward.md) compares per
  peer.
- Keep this separate from the `.internal/origins` dial gossip
  (`rs/moq-relay/src/cluster.rs`): deployments such as moq.pro dial from
  static config and leave gossip off, but still need the graph.

Test with an in-process four-relay ring. All four settle on one digest.
Closing one session changes the digest on every relay. A relay that publishes
no topology is absent from the graph, and so are its links.

## Related

- [Tree-routed announcements](/quest/m1/announce-tree/README.md) - the rule
  this graph feeds
