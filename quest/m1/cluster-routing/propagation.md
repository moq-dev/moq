# [M] Plan announcement propagation

## Goal

Decide how an announcement reaches every relay in a cluster once, rather than
once per neighbour, and how every relay keeps the ledger of live announcements
that ANNOUNCE_REQUEST and a cold SUBSCRIBE need. The output is a decision
recorded in the [questline README](/quest/m1/cluster-routing/README.md),
worked counterexamples, and rewritten implementation quests, not production
code.

## Plan

Decided: every relay holds a record for every live announcement, and an
announcement names its origin relay and cost, not a path to it
([Topology](/quest/m1/cluster-routing/topology.md) supplies reachability).

Candidates to choose between or combine:

- **Tree flooding with per-origin seqnos.** The README's candidate design:
  existence floods along the origin's shortest-path tree, a relay forwards an
  event only when it changes its view, and per-origin seqnos with
  incarnation resets stop stale revivals. This is the same as "skip a
  neighbour already closer to the origin": a relay forwards only to its
  children in that origin's tree.
- **A central node owns gossip.** Registries: an optional tier, one or more
  per region, full-meshed among themselves. An ingest relay registers with
  its nearest, an edge sends its ANNOUNCE_REQUEST there, and a relay fails
  over to the next-nearest and reconciles (its full live set at its current
  seqno, ending whatever it leaves out; the registry's snapshot carries each
  origin's last reconcile seqno). With none reachable a relay freezes its
  view and alerts. The simulator found registries buy cross-ocean bytes and
  one-round-trip announce latency, not message count, at live's 34 relays.
- **A compressed ledger.** The records are a replicated ledger of every live
  announcement; sync it with digests (anti-entropy) and compress it, such as
  prefix-shared paths, instead of per-event messages.

Measure the choice with the simulator's scenarios (messages and bytes per
event, convergence, stale revival, restart, failover) and the
[Memory benchmark](/quest/m1/cluster-routing/memory.md)'s figures.

Open questions the chosen design must answer:

- How long a relay keeps an ended path's seqno. A new origin incarnation
  clears it; within one, it must outlive every delayed copy of the start.
- How a relay that just became another's tree child catches up on what it
  missed within the same incarnation. Its reset only marks a new incarnation,
  so the push may need a watermark like the registry reconcile's: it ends only
  what the child holds at or below it, and a delayed push cannot erase newer
  records.
- How long a relay keeps the records of an origin that never comes back. They
  stop being reported once it is unreachable, but nothing removes them.
- Whether registries belong in this line. If deferred, write an m2 quest for
  them with sharding (HRW over a prefix key) and a mixed-version bridge as its
  open questions, and one for reduced flooding
  ([RFC 9667](https://www.rfc-editor.org/rfc/rfc9667)).

The implementation quests it writes replace the empty-prefix ANNOUNCE_REQUEST
between peers and per-hop path-vector announcements, drop `Hop Base` and
`Hop Keep` from the current wip lite version, and report the memory
benchmark's after figures. Add each one to the Required of
[Selection](/quest/m1/cluster-routing/selection.md) and
[Routing between clusters](/quest/m1/cluster-routing/inter-cluster.md), so
neither starts on a record shape that is not implemented yet.

## Required

- [Memory benchmark](/quest/m1/cluster-routing/memory.md) - the before figures the choice is weighed against

## Related

- [Cross-relay delivery under bursts](/quest/m1/cross-relay-bursts.md) - its report of closed broadcasts announced for minutes is evidence for per-origin seqnos
- [Announcement shapes](/quest/m2/announce-shapes.md) - announcements between relays must keep their shape
