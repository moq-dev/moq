# [L] Sparse overlay

## Goal

A mesh relay node keeps direct links to about log N of its directly reachable peers
instead of all of them, chosen deterministically so every node computes the
same overlay from the member list, and forwards for the rest. Any two nodes in
a reachable group reach each other through the overlay in about log N hops,
a departed node's links heal, an optional per-node degree cap bounds a
constrained robot's links, and traffic crosses the relay uplink, which every
relay node keeps, when the overlay partitions. Every node is a core unless
its application marks it an edge: an edge links to the cores it can reach
and its links cost more, so traffic between edges crosses a core while one
is reachable, and edges ring among themselves when none is. An application
embedding `Endpoint` reaches the overlay through a relay node.

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
- ~~Nodes that do not forward.~~ Superseded 2026-11-10 by the roles below:
  Kixel dropped its `--leaf` flag, and every node forwards.

Decided 2026-11-10 (maintainer, /quest-plan on lite-07 routing, asked how a
beefier node should attract traffic once Kixel's non-forwarding leaf is
gone): ✅ **two roles, core and edge, and both link shaping and link
pricing.** In the maintainer's words: "Kinda like how moq.pro wants to have
edges and cores. Edges should prioritize connecting to cores, but if there's
no cores, do a ring of edges? I just want this to work by default, which I
guess means everything is a core unless stated otherwise." Rejected: pricing
alone; shaping alone; ranking links to cheaper nodes by priority, whose
outcome under the feasibility rule depends on which route arrived first.
That reason was written under Babel. Under the path rule
([routes epic](/quest/m2/cluster-routes/README.md) Plan item 27) a
relay takes the cheapest route whose path does not name it, whatever arrived
first.

- Every node is a core unless the embedding application says it is an edge.
  Every node forwards; the role only changes which links it keeps and what
  they cost.
- Shaping: cores ring among themselves as below. An edge links to the cores
  it can reach, and rings with other edges only when it reaches none, so the
  mesh works with no core at all. Settle in the PR what an edge that reaches
  a core does for an edge that reaches none (partial reach); recommended: it
  accepts that edge's link.
- Pricing: a node declares a cost, 1 for a core and more for an edge, and a
  link costs the product of its two ends' costs. A cost added per hop cannot
  move traffic onto a core: a direct link is always cheaper than the same
  link plus another. With a core at 1 and an edge at 3, edge to edge costs 9
  and edge to core to edge costs 6, whichever route arrived first. This is
  local pricing of each peer's declared cost
  ([link costs](/doc/bin/relay/cluster.md#link-costs)) and needs no wire
  change; confirm that, and the edge's default cost, in the PR.
- Recommended, to confirm in the PR: an edge marks its links to cores
  `upstream`, as moq.pro's edges do, so it never carries one core's routes to
  another.

- Ring: order the reachable peers (found through mDNS or
  [address hints](/quest/m3/mesh/addr.md)) by a hash of their endpoint ID, and
  link to the first peer at or after distance 2^k for each k. Accept incoming
  links from peers whose ring picks this node. A target that cannot be dialed
  is replaced by the next peer on the ring.
- Degree cap: an optional per-node setting for the maximum direct links,
  unset by default; the ring alone decides when it is unset. Over the cap,
  drop the longest fingers first. Small meshes come out close to full mesh on
  their own.
- Forwarding and route choice are [cluster routing](/quest/m3/cluster-routing/README.md)'s:
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
Update `doc/bin/relay/cluster.md` and the [guide](/quest/m5/mesh-guide.md) if it exists.

## Required

- [Mesh relay node](/quest/m3/mesh/node.md) - the enrolled relay node that dials the overlay

## Related

- [Address hints](/quest/m3/mesh/addr.md) - a second source of reachable peers
- [Link quality](/quest/m5/link-quality.md) - measured link cost a fading overlay link could use
