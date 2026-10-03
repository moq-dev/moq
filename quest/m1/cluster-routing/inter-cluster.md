# [M] Routing between clusters

## Goal

Announcements crossing a cluster boundary stay path vector with configured
cluster domains (`cluster.domain` / peer `cluster`) as the hops, like BGP
between autonomous systems. A customer's on-prem
cluster is one hop, and an announcement naming the receiving cluster is
dropped. Inside a cluster, hop lists name relays; across a boundary they name
clusters.

## Plan

- Decided with the maintainer on 2026-10-02: a boundary is a configured link
  with an expected remote cluster identity, distinct from the local cluster
  identity. Preserve `cluster.id` as the existing relay-origin Hop; silently
  making every relay share that ID would merge distinct origins.
- Add a stable, explicit local cluster identity and an expected cluster
  identity on each configured boundary peer. Recommended config spelling:
  `cluster.domain` for the local identity and `cluster` on the existing peer
  object for the remote identity. Use nonzero identifiers below 2^62, in a
  separate role from relay Hop IDs. Every member of a cluster shares the
  configured domain; never mint one randomly per restart.
- Existing peers without a boundary identity remain intra-cluster. Refuse a
  boundary configuration before listening or dialing if its local domain is
  missing, its peer domain equals its local domain, or either identity is malformed. Keep the existing
  peer authentication requirement; a URL or an observed socket address alone
  does not authorize a boundary peer.
- Validate the remote domain against the configured expected identity when
  establishing the boundary link. A missing, mismatched, or unconfigured
  boundary identity is fatal. Both link directions must be classified
  explicitly; do not trust an arbitrary domain supplied in an announcement.
  Bind the expected domain to the authenticated link on accepted and dialed
  sessions: the existing `peer` grant alone does not identify a domain.
  Carry the identity declaration in the current unpublished lite version's
  boundary handshake, with no change to published versions. Refuse boundary
  operation when that version is not negotiated. This is validation of the
  configured identities, not automatic topology discovery.
- What crosses a boundary is a cluster's announcement, plus its cluster-domain
  list. An imported record keeps that list as provenance, so whichever boundary
  relay exports it appends its configured `cluster.domain` to the full list.
  Boundary path-vector hops use these domains; intra-cluster hop lists keep
  using relay Hop IDs (`cluster.id`). Stripping provenance on import would
  let A → B → C → A re-enter A.
- The remote origin is outside the receiving cluster, so the
  importing boundary relay announces the record inside its cluster as the
  origin, with the boundary link's cost added. The serving origin's identity
  still rides the reply, so re-originating does not merge two sources.
- Cluster-domain lists are short, so they need no `Hop Base`/`Hop Keep`
  compression.
- Cost across the boundary is plain configured link cost; business policy
  and incomparable costs are
  [Routing cost domains](/quest/m3/routing-cost-domains.md)'s.

Wire: the boundary announcement in the current wip lite version, with the
draft updated in the same PR. Tests: two clusters with two boundary links
and an origin that is not a boundary relay, and three clusters in a cycle
with different ingress and egress relays, where an announcement neither loops
nor re-enters its origin cluster.

Also test malformed and missing configuration, mismatched peer identities,
unauthenticated boundary requests, version refusal, and an ordinary
intra-cluster peer that retains its existing behavior. Include an authenticated
peer claiming the wrong domain. Sweep the routing
benchmark over clusters and boundary links, including a path-vector cycle,
so forwarding cost is measured against the touched path rather than the
whole route table.

Update `doc/bin/relay/cluster.md`'s configuration and CLI examples inline.
The parent quest already owns the new concept routing page, so no separate
documentation quest is needed.

Public API: additive explicit cluster-domain and boundary-peer configuration.
Wire: identity declaration and boundary announcements in the unpublished
lite version only; published versions do not gain boundary operation.

## Related

- [Routing cost domains](/quest/m3/routing-cost-domains.md) - designs policy on this path vector
