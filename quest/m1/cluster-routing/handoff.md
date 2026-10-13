# [M] Next-hop handoff

## Goal

When a node's next hop toward an origin changes, the announces it treats as
live stay current: it adopts a replacement neighbour's stored set only once
that set is complete, and an origin's ANNOUNCE_END stays final across the
handoff, even when the END and the next-hop change cross in flight. The
chosen design, any wire it needs, and its deterministic acceptance tests are
recorded in [Routes and announces](/quest/m1/cluster-routing/routes.md)
before that wire is written.

## Plan

Next-hop authority alone measured about twice the once-only minimum of
client updates when a broadcast ends, and on an upstream link loss the
announces trailed the route move (see the simulator findings in Routes and
announces).

Compare two designs in time-mocked tests:

- A snapshot barrier: a neighbour marks when the announces it holds for a
  route are complete, and a node adopts a new next hop's set only after that
  mark, keeping the old set until then.
- END generations or tombstones per origin that no stale neighbour's copy
  can outlive.

Bring the comparison to the maintainer, then record the choice in
routes.md's wire sketch and Tests.

Acceptance, time mocked: a new next hop whose ROUTE arrives before its
ANNOUNCE set; an END crossing a next-hop change; a ring of three where a
stale neighbour cannot revive an ended broadcast; per-path retractions and
re-announcements counted beside the ROUTE budget, against one update per
node for an end.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the route layer whose next-hop authority this completes
