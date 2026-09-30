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

moq.pro's routing simulator (`just rs sim`,
[moq.pro#2020](https://github.com/moq-dev/moq.pro/pull/2020)) measured worse
than that on live's relay graph (34 relays, mean degree 8.4): a publish costs
about 750 announces, and ending a path's only publisher explores stale
alternatives for about a second and 39k to 80k announces, with subscribes
looping meanwhile. Coalescing each writer for 10 ms cuts that by 7x and leaves
the loops. Hiding the routes through a peer that withdrew the path
([#4399](https://github.com/moq-dev/moq/pull/4399)) reduces that path hunting
but does not end it; see the findings below.

### Decisions

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
- The topology is configured: `--cluster-connect` or the connect API gives the
  relay graph and link costs. Relays flood per-link liveness among themselves
  with a per-link seqno. The seqno is scoped to the relay's incarnation, so a
  restarted relay's links supersede its stale ones instead of looking older.
  Configured links are the only source: gossip discovery is removed by
  [Remove gossip](/quest/m0/remove-gossip.md), and LAN mDNS dials peers
  that then count as configured links.
  - A relay batches the liveness reports it sends, its own and those it
    forwards, for a short hold-down (50 ms in the simulator), and recomputes
    its trees after a matching delay, as OSPF's SPF delay does. Unbatched, one
    relay restart at 340 relays sent half a million messages; batched, 27k.
  - On session up, relays exchange a digest (each reporter's incarnation and
    its seqno per link) and send only what the other lacks. A reporter's
    reports flood separately, so its newest seqno alone would hide a missing
    older one. The digest already counts the fresh report of the link that
    just came up; sending the database first replays the relay's own report
    from when the link went down, and the peer drops the link it is using.
- A relay picks the origin with the lowest shortest-path distance plus origin
  cost, ties broken by rendezvous hashing (HRW) of the requested path and the
  origin id, and forwards along its shortest path. Distance compares cost,
  then hop count, so every hop strictly shortens it even across `?cost=0`
  links. That is a shortest path to a virtual node linked to every origin, so
  it is loop-free whenever relays agree on the topology. The longest covering
  prefix still ranks first, per [Wildcard](/quest/m0/wildcard/README.md). The
  simulator saw no loop while views agreed, and HRW split an equal-cost pool
  63/49 where today's hash of the announced prefix sends all of it to one
  sibling.
- The first relay's choice rides the SUBSCRIBE, and transit relays forward
  toward that origin by topology alone, never re-selecting. Re-selection
  against another existence view loops: a relay that lost a specific claim
  falls back to a broader one through a relay still routing to the specific
  one ([RFC 8966 section 3.5.4](https://www.rfc-editor.org/rfc/rfc8966#section-3.5.4)).
  If the origin no longer serves the path, it refuses, and the first relay
  selects again.
- SUBSCRIBE and FETCH carry a visited-relay list end to end. It catches loops
  while liveness views disagree and names the path for stats. Narrowing it to
  cluster hops is later work. The serving origin's identity rides the reply,
  per Wildcard's Spread quest. On live's graph no disagreement looped across
  20 seeds of link, cost, and relay churn; the list is a safety net.
- Announcements are on demand. A relay forwards only the union of its clients'
  ANNOUNCE_REQUEST prefixes, never the empty prefix. A wide prefix that many
  edges' viewers request is that customer's cost. `.stats` becomes ordinary
  demand.
- Registries are an optional, configured tier: moq-relay in a registry mode,
  one or more per region.
  - An ingest relay registers its broadcasts with its nearest registry, and an
    edge sends its ANNOUNCE_REQUEST there.
  - Registries form a small full mesh and flood existence among themselves, so
    an event crosses an ocean once per remote registry, not once per relay.
    Announce latency is about one round trip to the nearest registry,
    whatever the path length.
  - A relay fails over to the next-nearest registry and reconciles its view
    instead of treating the lost session as ends, so a registry failure never
    reports a live broadcast offline.
  - The reconcile is the relay's full live set at its current seqno, which
    ends whatever it leaves out. The registry's snapshot carries each
    relevant origin's last reconcile seqno, and the relay names the origins it
    already holds, so a view kept through a freeze ends what ended meanwhile.
  - With no registry reachable, a relay freezes: it keeps its view, learns
    nothing new, and alerts. Falling back to flooding would cascade the
    failure.
- Without registries (self-hosting), existence floods along the shortest-path
  tree, one copy per relay. A relay forwards an event only when it changes its
  view, so a duplicate copy, from trees built on disagreeing liveness, stops
  there. A relay that gains a child in its tree (a view change or a new
  session) pushes that origin's reset and records to it; without the push a
  relay misses events while views disagree.
- Between clusters, announcements stay path vector with cluster ids as the
  hops, like BGP between autonomous systems. A customer's on-prem cluster is
  one hop, and an announcement naming the receiving cluster is dropped.
- The cluster switches versions as a whole; older lite and IETF sessions stay
  at its edges.

### Simulator findings

The report is moq.pro's `just rs sim` (every scenario on live's graph) and
`just rs sim sweep` (synthetic regional graphs of 34, 340, and 1020 relays).
It carries messages and bytes by kind, per relay and cross-region,
convergence, loop, stall, and failover windows, and state per relay and per
registry. What decides the wire:

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
  per relay. A reduced flooding topology
  ([RFC 9667](https://www.rfc-editor.org/rfc/rfc9667)) is the known fix for
  the flooding.
- Failure detection, not routing, sets every outage window: a silent link or
  relay loss is noticed after the 30 s QUIC idle timeout in every candidate,
  and subscribes through it go nowhere until then. Cluster sessions need a
  short idle timeout; a keepalive only keeps a quiet session open.
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

### Open questions

- Sharding registries by HRW over a prefix key once one registry cannot hold
  everything, and what that key is.
- A mixed-version bridge, if a fleet cannot switch at once.
- How long a relay keeps an ended path's seqno. A new origin incarnation
  clears it; within one, it must outlive every delayed copy of the start.
- How a push to a new tree child ends what the child missed within the same
  incarnation. Its reset only marks a new incarnation, so the push may need a
  watermark like the registry reconcile's: it ends only what the child holds
  at or below it, and a delayed push cannot erase newer records.
- How long a relay keeps the records of an origin that never comes back. They
  stop being reported once it is unreachable, but nothing removes them.
- How an edge routes a SUBSCRIBE for a path none of its clients asked to
  announce. It holds no route for it, and asking a registry first adds a round
  trip before the first byte.
- What a cold ANNOUNCE_REQUEST reports as live. Answering from the local view
  keeps a relay from waiting on peers but reports an empty set until the
  registry's replay lands, on every new prefix rather than in a rare race.
- What remains of announce compression's hop-tail half (`Hop Base` and
  `Hop Keep` in the lite draft) once only cluster boundaries carry hops.
- What replaces `--hop` first-hop failover. Today two publishers sharing a Hop
  ID are one source that relays fail over between at a group boundary
  (`doc/bin/cli.md` "Redundant publishers",
  `doc/concept/use-case/contribution.md`). Inside a cluster no announcement
  carries a hop list, so two encoders on different ingest relays become two
  origins. Keep the documented behavior or change the docs in the same PR.
- Whether equal-cost next hops should spread by a hash of the path. A fixed
  tie-break sends every path through the same neighbour and its failure takes
  them all.

## Required

- moq.pro workers stop electing on hop chains, reading the relay's local origin instead ([moq.pro voice-local-origin](https://github.com/moq-dev/moq.pro/blob/main/quest/m0/voice-local-origin.md))
- [Wildcard](/quest/m0/wildcard/README.md) - the longest-prefix rule, pool spread, and reply identity this selection builds on

## Related

- [Redundant ingest](/quest/m2/redundant-ingest.md) - builds on the `--hop` failover this must keep or replace
- [Routing cost domains](/quest/m2/routing-cost-domains.md) - cost across the cluster boundaries this keeps path vector
- [Cross-relay delivery under bursts](/quest/m1/cross-relay-bursts.md) - its #4349 report also shows closed broadcasts announced for up to 229 s and flapping between Retracted and Announced across nodes, evidence for per-incarnation seqnos
