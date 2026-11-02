# [L] Sparse overlay

## Goal

A mesh node keeps direct links to about log N of its directly reachable peers
instead of all of them, chosen deterministically so every node computes the
same overlay from the member list, and forwards for the rest. Any two nodes in
a reachable group reach each other through the overlay in about log N hops,
a departed node's links heal, an optional per-node degree cap bounds a
constrained robot's links, and traffic crosses the relay uplink, which every
node keeps, when the overlay partitions.

## Plan

Decided 2026-10-31 (maintainer, moq.pro mesh planning interview):
deterministic and local, not coordinator-assigned, so self-hosted meshes get
it; the coordinator is only the member list (announcements under `mesh/**`)
and the fallback path.

- Ring: order the reachable peers (found through mDNS or
  [address hints](/quest/m2/mesh/addr.md)) by a hash of their endpoint ID, and
  link to the first peer at or after distance 2^k for each k. Accept incoming
  links from peers whose ring picks this node. A target that cannot be dialed
  is replaced by the next peer on the ring.
- Degree cap: an optional per-node setting for the maximum direct links,
  unset by default; the ring alone decides when it is unset. Over the cap,
  drop the longest fingers first. Small meshes come out close to full mesh on
  their own.
- Forwarding and route choice are [cluster routing](/quest/m1/cluster-routing/README.md)'s:
  each daemon is already a relay, overlay links are LAN-cost links, and relay
  uplinks stay `upstream` links costed above them. Nothing routes outside the
  shared priority/cost rules.
- Membership churn recomputes the ring; hold a change briefly so a flapping
  node does not churn every link, and record the hold chosen.

Tests in network namespaces on one segment: each node's direct link count
follows the ring (and never exceeds the cap when set); every pair connects
without crossing the relay; killing a node heals the overlay; splitting the
segment sends cross-half traffic through the relay and healing restores the
direct path. Report the link counts and hop counts the test measures.
Update `doc/bin/cli.md` and the [guide](/quest/m2/mesh/guide.md) if it exists.

## Required

- [Daemon](/quest/m2/mesh/daemon.md) - the relay that dials the overlay

## Related

- [Address hints](/quest/m2/mesh/addr.md) - a second source of reachable peers
- [Link quality](/quest/m2/link-quality.md) - measured link cost a fading overlay link could use
