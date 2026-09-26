# [XL] Cluster routing

## Goal

A broadcast event reaches each relay at most once, and a relay learns only the
prefixes its own clients asked for. An announcement says a path exists at an
origin relay, at a cost; how to reach that origin comes from a shared relay
topology, so no announcement inside a cluster carries a hop list. This quest
records the design; its wire and implementation quests are planned from the
simulator's report.

Non-goals: warm re-origination (a warm relay would be one more origin with a
cost, so leave room for it), and a permanently mixed-version cluster.

## Plan

### Why not path vector or Babel

Today every relay advertises its best route to every peer not already in the
hop chain. One publish costs about R·(d-1) announces for R relays of mesh
degree d, every relay learns every broadcast (`.stats` and `.internal`
included), and a link change rewrites every route crossing it. On moq.pro's
live fleet (26 PoPs, average degree about 5) each relay receives every event
about five times. Babel ([RFC 8966](https://www.rfc-editor.org/rfc/rfc8966))
shrinks the message and skips equal-cost reroutes, but it is still distance
vector: the same fan-out, the same global knowledge, and no loop freedom
when several sources claim a prefix (section 2.7), which pools and wildcards
make MoQ's common case.

### Decisions

- Existence is split from reachability. An announcement carries the path, its
  origin relay, and the origin's cost, and nothing about the path to it.
- The topology is configured: `--cluster-connect` or the connect API gives the
  relay graph and link costs. Relays flood per-link liveness among themselves
  with a per-link seqno. Gossip discovery (`cluster.mesh`) stays for
  zero-config self-hosting and derives the topology from what it discovers; it
  need not scale.
- A relay picks the origin with the lowest shortest-path distance plus origin
  cost, ties broken by rendezvous hashing (HRW) of the requested path and the
  origin id, and forwards along its shortest path. That is a shortest path to
  a virtual node linked to every origin, so it is loop-free whenever relays
  agree on the topology. Specificity still ranks first, per
  [Wildcard](/quest/m0/wildcard/README.md).
- SUBSCRIBE and FETCH carry a visited-relay list end to end. It catches loops
  while liveness views disagree and names the path for stats. Narrowing it to
  cluster hops is later work. The serving origin's identity rides the reply,
  per Wildcard's Spread quest.
- Announcements are on demand. A relay forwards only the union of its clients'
  ANNOUNCE_REQUEST prefixes, never the empty prefix. A wide prefix that many
  edges' viewers request is that customer's cost. `.stats` becomes ordinary
  demand.
- Registries are an optional, configured tier: moq-relay in a registry mode,
  one or more per region.
  - An ingest relay registers its broadcasts with its nearest registry, and an
    edge sends its ANNOUNCE_REQUEST there.
  - Registries form a small full mesh and flood existence among themselves, so
    an event crosses an ocean once per region. Announce latency is about one
    round trip to the nearest registry, whatever the path length.
  - A relay fails over to the next-nearest registry and reconciles its view
    instead of treating the lost session as ends, so a registry failure never
    reports a live broadcast offline.
  - With no registry reachable, a relay freezes: it keeps its view, learns
    nothing new, and alerts. Falling back to flooding would cascade the
    failure.
- Without registries (self-hosting), existence floods along the shortest-path
  tree, one copy per relay.
- Between clusters, announcements stay path vector with cluster ids as the
  hops, like BGP between autonomous systems. A customer's on-prem cluster is
  one hop, and an announcement naming the receiving cluster is dropped.
- The cluster switches versions as a whole; older lite and IETF sessions stay
  at its edges.

### Open questions

- Sharding registries by HRW over a prefix key once one registry cannot hold
  everything, and what that key is.
- A mixed-version bridge, if a fleet cannot switch at once.
- What remains of [Announce compression](/quest/m1/announce-compression.md)'s
  hop-tail half once only cluster boundaries carry hops.

## Required

- moq.pro's routing simulator reports ([quest](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/routing-simulator.md))

## Related

- [Skip unchanged announce updates](/quest/m0/announce-update-dedupe.md) - cuts duplicate updates on today's routing
- [Local origin](/quest/m0/local-origin.md) - workers stop reading hop chains before they go
- [Wildcard](/quest/m0/wildcard/README.md) - the specificity, pool spread, and reply identity this selection builds on
