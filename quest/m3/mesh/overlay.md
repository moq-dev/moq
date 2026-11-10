# [L] Sparse overlay

## Goal

A mesh relay node keeps direct links to about log N of its directly reachable peers
instead of all of them, chosen deterministically so every node computes the
same overlay from the member list, and forwards for the rest. Any two nodes in
a reachable group reach each other through the overlay in about log N hops,
a departed node's links heal, an optional per-node degree cap bounds a
constrained robot's links, and traffic crosses the relay uplink, which every
relay node keeps, when the overlay partitions. An application embedding
`Endpoint` reaches the overlay through a relay node.

## Plan

Decided 2026-10-31 (maintainer, moq.pro mesh planning interview):
deterministic and local, not coordinator-assigned, so self-hosted meshes get
it; the coordinator is only the member list (announcements under `mesh/**`)
and the fallback path.

Decided 2026-11-09 (maintainer, /quest-plan from Kixel's audit of its fleet
plan): ✅ the ring stays the link choice, with no measured cost and no exit
designation (rejected: links chosen by measured proximity; a coordinator
computing the graph), on the working assumption that a peer is either on the
same network or out of reach. Two cases the ring as written does not cover
join this quest:

- Partial reach. On radios A reaches B and B reaches C while A cannot reach
  C, so each node computes its ring over a different set of peers. Test it:
  in namespaces with filtered links (a line and a grid), every pair with a
  path connects without crossing the relay, a flapping link reroutes without
  a loop, and the links and hops are reported. A node with no uplink has no
  fallback, so report whether the overlay stays connected whenever the reach
  graph is, and record what such a node can know about being cut off.
- Nodes that do not forward. An embedding application may turn a node's
  `proxy` preference off so it carries only its own traffic (a camera or a
  constrained robot; Kixel plans a `--leaf` flag for it). Such a node cannot
  be a finger that forwards for the rest. Recommendation, to confirm in the
  PR: it is not a ring member, links to a small number of members, and is
  never chosen as a finger. Expose that preference per node in the relay
  library if the embedding application cannot set it today.

- Ring: order the reachable peers (found through mDNS or
  [address hints](/quest/m3/mesh/addr.md)) by a hash of their endpoint ID, and
  link to the first peer at or after distance 2^k for each k. Accept incoming
  links from peers whose ring picks this node. A target that cannot be dialed
  is replaced by the next peer on the ring.
- Degree cap: an optional per-node setting for the maximum direct links,
  unset by default; the ring alone decides when it is unset. Over the cap,
  drop the longest fingers first. Small meshes come out close to full mesh on
  their own.
- Forwarding and route choice are [cluster routing](/quest/m2/cluster-routing/README.md)'s:
  overlay links join relay nodes as LAN-cost links, and relay
  uplinks stay `upstream` links costed above them. Nothing routes outside the
  shared priority/cost rules.
- Membership churn recomputes the ring; hold a change briefly so a flapping
  node does not churn every link, and record the hold chosen.

Tests in network namespaces, on one segment beside the partial-reach layouts
above: each node's direct link count
follows the ring (and never exceeds the cap when set); every pair connects
without crossing the relay; killing a node heals the overlay; splitting the
segment sends cross-half traffic through the relay and healing restores the
direct path. Report the link counts and hop counts the test measures.
Update `doc/bin/relay/cluster.md` and the [guide](/quest/m3/mesh/guide.md) if it exists.

## Required

- [Mesh relay node](/quest/m3/mesh/node.md) - the enrolled relay node that dials the overlay

## Related

- [Address hints](/quest/m3/mesh/addr.md) - a second source of reachable peers
- [Link quality](/quest/m5/link-quality.md) - measured link cost a fading overlay link could use
