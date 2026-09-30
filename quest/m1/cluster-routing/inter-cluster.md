# [M] Routing between clusters

## Goal

Announcements crossing a cluster boundary stay path vector with cluster ids
as the hops, like BGP between autonomous systems. A customer's on-prem
cluster is one hop, and an announcement naming the receiving cluster is
dropped. Inside a cluster nothing carries a list of relay hops.

## Plan

- A boundary is a configured link to a relay of another cluster; decide how
  a relay knows its own cluster id and its peer's.
- What crosses a boundary is what the chosen
  [Propagation](/quest/m1/cluster-routing/propagation.md) design holds, plus
  its cluster-id list. An imported record keeps that list as provenance, so
  whichever boundary relay exports it appends its own cluster id to the full
  list; stripping it on import would let A → B → C → A re-enter A.
- The remote origin is outside the receiving cluster's topology, so the
  importing boundary relay announces the record inside its cluster as the
  origin, with the boundary link's cost added. The serving origin's identity
  still rides the reply, so re-originating does not merge two sources.
- Cluster-id lists are short, so they need no `Hop Base`/`Hop Keep`
  compression.
- Cost across the boundary is plain configured link cost; business policy
  and incomparable costs are
  [Routing cost domains](/quest/m3/routing-cost-domains.md)'s.

Wire: the boundary announcement in the current wip lite version, with the
draft updated in the same PR. Tests: two clusters with two boundary links
and an origin that is not a boundary relay, and three clusters in a cycle with different ingress and egress relays, where
an announcement neither loops nor re-enters its origin cluster.

## Required

- [Propagation](/quest/m1/cluster-routing/propagation.md) - the record shape a boundary exports

## Related

- [Routing cost domains](/quest/m3/routing-cost-domains.md) - designs policy on this path vector
