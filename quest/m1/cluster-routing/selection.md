# [L] Deterministic origin selection

## Goal

A broadcast under overlapping announcements routes to one origin chosen
deterministically, and a relay spreads paths over equally ranked next hops
(an edge over its region's cores) so every relay sends a given path the same
way.
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

- Route selection stays `route_order` over the routes a relay holds: longest
  prefix, then cost, then a rendezvous hash (HRW) of the requested path and
  the route's origin. At an edge that spreads paths over the region's cores
  and makes every edge pick the same core for a path; failover moves only the
  paths the lost core won.
- Every hop re-selects (decided 2026-10-01): SUBSCRIBE names no origin, since
  a pin breaks subscription aggregation. Once
  [Routes and announces](/quest/m1/cluster-routing/routes.md) lands, the
  candidates are the origin nodes announcing the path, ranked by longest
  prefix, then route metric to the node, then HRW, and the reply's Origin is
  the serving node id. Write the ranking so that change swaps its inputs, not
  its shape.
- A refusal follows Wildcard's refusal rule: every refusal is terminal, and
  an origin sheds load by withdrawing or re-pricing its route instead.
- The serving origin's identity rides the Origin field of the reply, per
  Wildcard's resume rule.

Open:

- How a relay tells claim output from a redundant pair. One candidate: a
  concrete path an origin announces under its own claim keeps per-origin
  identity, since a redundant publisher claims nothing.

Wire: none expected; if one is needed it goes in the current wip lite
version with the draft. Tests cover an HRW split across an equal-cost
pool, refusal and reselection, a same-epoch pair failing over mid-track
with no timestamp rewind, and a concrete double claim whose loser's
subscribers end and resubscribe rather than splice.

## Required

- [Upstream links](/quest/m1/cluster-routing/transit.md) - the edges and cores this spreads over
- [Wildcard](/quest/m0/wildcard/README.md) - the longest-prefix rule, pool spread, and reply identity this builds on
- [Epoch primitive](/quest/m1/epoch.md) - parses the `@<uuidv7>` segment that makes a path a source identity
