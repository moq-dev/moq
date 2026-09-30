# [L] Deterministic origin selection

## Goal

A broadcast under overlapping announcements routes to one origin chosen
deterministically, and every relay forwards toward that origin by topology.
Origins that announce the same epoch-qualified concrete path are one source,
so a subscriber moves between them at a group boundary when the incumbent
ends or becomes unreachable.

## Plan

Decided:

- The longest covering prefix ranks first, per
  [Wildcard](/quest/m0/wildcard/README.md), and origin choice is
  per broadcast, not per announced prefix.
- An epoch-qualified concrete path (`foo/@<uuidv7>`) is a source's identity:
  every origin announcing it is interchangeable, which is how a redundant
  pair works without `--hop`. The first relay fails over between them at a
  group boundary. A path with no epoch, or one a claim produces, keeps
  Wildcard's per-origin identity (the origin SUBSCRIBE_OK names), since two
  workers' groups differ. That includes a claim worker's derived output once
  it is announced concretely: it mirrors the input's epoch
  (`.pro/transcode/<pid>/foo.hang/@e`), so after a double claim two workers
  announce the same epoch-qualified path and must not pool. This extends
  Wildcard's resume rule to concrete same-epoch origins.

Candidate mechanics:

- A relay picks the origin with the lowest shortest-path distance plus origin
  cost, ties broken by rendezvous hashing (HRW) of the requested path and the
  origin id, and forwards along its shortest path. That is a shortest path to
  a virtual node linked to every origin, so it is loop-free whenever relays
  agree on the topology.
- The first relay's choice rides the SUBSCRIBE and FETCH, and transit relays
  forward toward that origin by topology alone, never re-selecting.
  Re-selection against another existence view loops: a relay that lost a
  specific claim falls back to a broader one through a relay still routing to
  the specific one
  ([RFC 8966 section 3.5.4](https://www.rfc-editor.org/rfc/rfc8966#section-3.5.4)).
  A refusal follows Wildcard's refusal rule: only a capacity refusal lets the
  first relay select once more within the same longest-prefix tier, excluding
  the refusing origin, and any other refusal is terminal.
- SUBSCRIBE and FETCH carry a visited-relay list end to end. It catches loops
  while liveness views disagree and names the path for stats. The serving
  origin's identity rides the reply, per Wildcard's first-hop resume rule.

Open:

- Whether equal-cost next hops should spread by a hash of the path. A fixed
  tie-break sends every path through the same neighbour, and its failure
  takes them all.
- How a relay tells claim output from a redundant pair. One candidate: a
  concrete path an origin announces under its own claim keeps per-origin
  identity, since a redundant publisher claims nothing.

Wire: SUBSCRIBE and FETCH fields in the current wip lite version, with the
draft updated in the same PR. Tests cover an HRW split across an equal-cost
pool, refusal and reselection, a same-epoch pair failing over mid-track
with no timestamp rewind, and a concrete double claim whose loser's
subscribers end and resubscribe rather than splice.

## Required

- [Topology](/quest/m1/cluster-routing/topology.md) - distance to each origin
- [Propagation](/quest/m1/cluster-routing/propagation.md) - the record shape that names each origin
- [Wildcard](/quest/m0/wildcard/README.md) - the longest-prefix rule, pool spread, and reply identity this builds on

## Related

- [Epoch primitive](/quest/m1/epoch.md) - parses the `@<uuidv7>` segment that makes a path a source identity
