# Cluster routing

## Goal

Cut cluster gossip. A relay learns how to reach each other relay once, from
a topology shared across the cluster rather than repeated in every route,
and learns each announcement once rather than once per neighbour. Every relay
still holds the ledger of live announcements that ANNOUNCE_REQUEST and a cold
SUBSCRIBE need, and a broadcast under overlapping prefixes routes to one
origin deterministically. Redundancy an operator configures may deliver more
than one copy.

Non-goals: warm re-origination (a warm relay would be one more origin with a
cost, so leave room for it), and a permanently mixed-version cluster.

## Plan

### Decisions

Settled in the 2026-09-30 `/quest-plan`:

- The goal stays wide. The design below is the current candidate, not a
  decision; [Propagation](/quest/m1/cluster-routing/propagation.md) settles
  how announcements move and writes the implementation children that follow.
- Topology is a cluster message kept apart from routes, built from configured
  links only (`--cluster-connect`, the connect API, LAN mDNS). Gossip
  discovery goes first, but only the topology child waits on it.
- Every relay holds a record for every live announcement, since
  ANNOUNCE_REQUEST and a SUBSCRIBE with no prior announce both need one.
  Scoping that knowledge by demand needs registries, which Propagation may
  defer to m2.
- `--hop` goes. An epoch-qualified concrete path (`foo/@<uuidv7>`) is the
  identity of a source: origins that announce the same one are
  interchangeable, which is how a redundant pair is expressed. It is strictly
  better than a Hop ID, which is per session, so a connection could not
  publish several broadcasts with different identities. A path a claim
  produces keeps Wildcard's per-origin identity, even once announced
  concretely, since each worker's output is its own.
- The line lands on `dev`: deleting `--hop` and the publisher's Hop setup
  parameter breaks a published CLI and wire. Wire changes go in the current
  wip version (`moq-lite-07-wip` today, dropping `Hop Base` and `Hop Keep`
  before they publish). If lite-07 is finalized first for the Wildcard
  rollout, the remaining children move to the next wip version; finalizing
  never waits on this line.
- The before/after memory figure is a committed benchmark, measured first.
- Failure detection is the QUIC idle timeout, outside this line; every
  routing design inherits its outage window.
- Reduced flooding ([RFC 9667](https://www.rfc-editor.org/rfc/rfc9667)) and
  registries are m2 unless Propagation pulls registries in; Propagation writes
  whichever it defers.
- Between clusters, announcements stay path vector with cluster ids as hops,
  in the last child.

### Why not path vector or Babel

Today every relay advertises its best route to every peer not already in the
hop chain, and pulls every peer's full table with an empty-prefix
ANNOUNCE_REQUEST. One publish costs about R·(d-1) announces for R relays of
mesh degree d, every relay learns every broadcast (`.stats` and `.internal`
included), and a link change rewrites every route crossing it. On moq.pro's
live fleet (26 PoPs, average degree about 5) each relay receives every event
about five times. Babel ([RFC 8966](https://www.rfc-editor.org/rfc/rfc8966))
shrinks the message and skips equal-cost reroutes, but it is still distance
vector: the same fan-out, the same global knowledge, and no loop freedom
when several sources claim a prefix (section 2.7), which pools and wildcards
make MoQ's common case.

moq.pro's routing simulator (`just rs sim`,
[moq.pro#2020](https://github.com/moq-dev/moq.pro/pull/2020)) measured worse
than that on live's relay graph (34 relays, mean degree 8.4): a publish costs
about 750 announces, and ending a path's only publisher explores stale
alternatives for about a second and 39k to 80k announces, with subscribes
looping meanwhile. Coalescing each writer for 10 ms cuts that by 7x and leaves
the loops. Hiding the routes through a peer that withdrew the path
([#4399](https://github.com/moq-dev/moq/pull/4399)) reduces that path hunting
but does not end it; see the findings below.

### Candidate design

The children own their parts: topology in
[Topology](/quest/m1/cluster-routing/topology.md), existence and registries
in [Propagation](/quest/m1/cluster-routing/propagation.md), origin choice in
[Selection](/quest/m1/cluster-routing/selection.md).

- Existence is split from reachability. An announcement carries the path, its
  origin relay, and the origin's cost, and nothing about the path to it.
- An existence event carries the origin's seqno, scoped to its incarnation. A
  relay applies an event only when it is newer than the last it applied for
  that path and origin, and keeps an ended path's seqno, so a start delayed on
  a stale tree or a failed-over registry cannot revive it. Babel keeps
  feasibility past withdrawal for the same reason
  ([RFC 8966 section 3.7.3](https://www.rfc-editor.org/rfc/rfc8966#section-3.7.3)).
- A new incarnation is learned from existence, never inferred from liveness:
  it announces a reset (its incarnation at seqno 0) that ends everything the
  old one published, and a relay applies a reset and the records after it as
  one batch. Incarnations are ordered, so a delayed reset from an older one is
  dropped. Purging on a liveness report instead drops a restarted origin's
  broadcasts until their re-announce lands, which the simulator showed as live
  broadcasts reported offline.
- Relays flood per-link liveness among themselves with a per-link seqno scoped
  to the relay's incarnation, batched for a short hold-down, with a digest
  exchange on session up.
- A relay picks the origin with the lowest shortest-path distance plus origin
  cost, ties broken by rendezvous hashing (HRW) of the requested path and the
  origin id, and forwards along its shortest path. The longest covering
  prefix still ranks first, per [Wildcard](/quest/m0/wildcard/README.md). The
  first relay's choice rides the SUBSCRIBE, and transit relays forward toward
  that origin by topology alone. SUBSCRIBE and FETCH carry a visited-relay
  list as a loop safety net.
- Announcements to clients are on demand: a relay forwards only the union of
  its clients' ANNOUNCE_REQUEST prefixes. `.stats` becomes ordinary demand.
- Registries are an optional, configured tier: moq-relay in a registry mode,
  one or more per region. Registries form a small full mesh and flood
  existence among themselves, so an event crosses an ocean once per remote
  registry. A relay fails over to the next-nearest registry and reconciles;
  with none reachable it freezes its view and alerts rather than falling back
  to flooding.
- Without registries, existence floods along the shortest-path tree, one copy
  per relay. A relay forwards an event only when it changes its view, and a
  relay that gains a child in its tree pushes that origin's reset and records
  to it.
- The cluster switches versions as a whole; older lite and IETF sessions stay
  at its edges.

### Simulator findings

The report is moq.pro's `just rs sim` (every scenario on live's graph) and
`just rs sim sweep` (synthetic regional graphs of 34, 340, and 1020 relays).
It carries messages and bytes by kind, per relay and cross-region,
convergence, loop, stall, and failover windows, and state per relay and per
registry.

- Existence costs about one message per relay per event flooded, and one per
  interested relay plus one per remote registry with registries. On live, a
  wide customer prefix with `.stats` from every relay cost 2.5M announces
  today, 4k flooded, and 3.5k with registries, whose relays held 63 records
  on average against 94.
- Liveness is the split's dominant cost at scale: flooding every link repeats
  each report once per neighbour. At 1020 relays (mean degree 35), a link
  loss and restore plus a relay loss and restart cost about 290 MiB of
  liveness flooding and 54 MiB of session-up digests against under 1 MiB of
  existence for twenty publishes, while a publish costs exactly one message
  per relay.
- Unbatched, one relay restart at 340 relays sent half a million liveness
  messages; batched for 50 ms, 27k.
- Failure detection, not routing, sets every outage window: a silent link or
  relay loss is noticed only after the QUIC idle timeout in every candidate,
  and subscribes through it go nowhere until then.
- The simulator saw no loop while views agreed, and HRW split an equal-cost
  pool 63/49 where today's hash of the announced prefix sends all of it to one
  sibling. On live's graph no disagreement looped across 20 seeds of link,
  cost, and relay churn.
- One registry per region and two cost about the same; two halves the
  busiest registry's load. A registration sent on a dead registry session
  that nobody has noticed yet waits for the failover.
- [#4399](https://github.com/moq-dev/moq/pull/4399) trades path hunting for
  flapping. In its own full mesh of 8 it retracts each of 100 withdrawn
  broadcasts exactly once per relay, as its tests show. On live's graph, with
  each writer coalescing for 1 ms, ending three paths loops subscribes for
  9 s instead of 41 s and edges report the ended paths for 38 s summed
  instead of 93, but it costs about the same 60k messages, and clients see
  17k updates instead of 12k, with ended paths re-announced 4185 times
  instead of 163: each re-announce by a peer revives every route through it,
  stale ones included. Written without coalescing, the revivals cascade, and
  the same scenario costs 18.6M messages instead of 189k. Per-origin seqnos,
  above, end both.

### Remaining work

Once every child has landed:

- An end-to-end test: a multi-relay cluster in `rs/moq-relay/tests` where a
  publish, an end, a relay restart, and a redundant-pair failover each reach
  a subscriber on a far relay, with time mocked.
- Rewrite `doc/bin/relay/cluster.md` into the operator's view (topology,
  link costs, idle timeout, redundant pairs), and add a routing page under
  `doc/concept`.

## Required

- [Memory benchmark](/quest/m1/cluster-routing/memory.md) - a committed benchmark states per-announcement, per-route, and per-peer relay memory, before anything changes
- [Topology](/quest/m1/cluster-routing/topology.md) - relays learn the relay graph once from a cluster message, apart from routes
- [Propagation](/quest/m1/cluster-routing/propagation.md) - decides how each announcement reaches every relay once, and writes the implementation children
- [Selection](/quest/m1/cluster-routing/selection.md) - a broadcast under overlapping prefixes routes to one origin deterministically, and same-epoch origins are one source
- [Remove `--hop`](/quest/m1/cluster-routing/hop-removal.md) - redundant publishers share an explicit `@<epoch>`, and `--hop` and the publisher's Hop ID are gone
- [Between clusters](/quest/m1/cluster-routing/inter-cluster.md) - announcements crossing a cluster boundary stay path vector with cluster ids as hops

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - the longest-prefix rule, pool spread, and reply identity Selection builds on
- [Same-hop importers](/quest/m1/hop-aligned-import.md) - the importer half of a redundant pair; `--hop` removal re-keys it to a shared epoch
- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - a redundant pair shares one epoch
- [Cross-relay delivery under bursts](/quest/m1/cross-relay-bursts.md) - its #4349 report also shows closed broadcasts announced for up to 229 s and flapping between Retracted and Announced across nodes, evidence for per-incarnation seqnos
- [Routing cost domains](/quest/m3/routing-cost-domains.md) - cost across the cluster boundaries this keeps path vector
