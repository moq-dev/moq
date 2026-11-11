# [L] The route model and router take an interest as an edited set of prefixes

## Goal

The route model (`rs/moq-net/verify/route/RouteHandoff.tla`) and the router
(`rs/moq-net/src/route/`) treat a neighbour as one session, keyed once for
both directions, whose interest is a set of prefixes edited in place. TLC
checks every ordering of an edit against the routes, batches, floors, and
echoes sharing one FIFO per direction, and the existing rules hold: an edit
that swaps prefixes for a wider or narrower one keeps every claim both cover,
ends nothing, revives nothing, and sends nothing twice. Nothing on the wire
or in the sessions changes here; [the route stream](/quest/m2/cluster-routing/routes/route-stream.md)
carries it.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on lite-07 routing). The model
goes first because the set's edit rule is the one new rule in this redesign,
and every earlier routing bug was an ordering nobody had enumerated.

Settled:

1. ✅ **A stamp covers one prefix, named by its id** (rejected: a stamp
   covering the whole set). A floor is `{route, prefix, revision}`, so each
   prefix is certified exactly as one stream per prefix was, and the merge
   rule is unchanged. A whole-set stamp needs the receiver to track which
   prefixes each stamp covers anyway, since a stamp sent before a prefix was
   added says nothing about it, and a sender that knows only part of the set
   could certify nothing. This amends Plan item 10 of the
   [epic](/quest/m2/cluster-routing/routes/README.md): the floor stays a
   marker, never a region list, and gains the prefix it names.
2. ✅ **SYNCED is per prefix, naming its id** (rejected: one SYNCED per edit,
   counted in order, which makes a small prefix wait behind a large one).
   Each edit also gets one marker in the answering side's stream, the point
   where it takes effect for both sides.
3. ✅ **The live set never holds a prefix inside another, and one edit may
   swap prefixes without re-sending what both cover.** In the maintainer's
   words: "we might have fleet/r7 and fleet/r2, then expand it to fleet ...
   somehow we should support fleet absorbing the previous two without
   retransmitting everything". Rejected: nested prefixes that each list
   independently, where a peer multiplies a relay's work by repeating a
   prefix, bounded only by message size; and removing the old prefixes only
   after the new one is SYNCED, which holds every claim twice during the
   handover and is what Plan items 16 and 17 of the epic did across streams.

The edit rule those three imply, to prove rather than assume:

- An edit removes live prefixes by id and adds prefixes, each taking the next
  id. An edit that would leave one live prefix inside another is refused.
- At the edit's marker, a claim of a removed prefix stays live, with its id
  and attributes, when an added prefix covers it and does not hide it; it is
  that prefix's claim from then on. Every other claim of a removed prefix is
  dropped. A claim presented at a removed prefix itself may stand for a
  broader claim (Plan item 11), which the receiver cannot tell apart, so it
  is always dropped and sent again if the new set still presents it.
- The sender then sends what the added prefixes newly cover, one floor per
  route for each added prefix, and SYNCED for it.
- Widening: a kept claim stays certified by its old prefix's last floor until
  its route's first floor for the new prefix. Narrowing: the wider floor
  carries to each added prefix inside it. Decide, and prove, what a floor
  that certifies nothing does to the narrower floors it replaces;
  recommended: it replaces them, so there is one floor per route per prefix.
- A route stays on the stream while some live prefix presents a claim of it
  (Plan item 15, over the set). One whose last claim an edit drops ends with
  ROUTE_END after the marker, echoed as usual, with no floor first, since
  nothing was withdrawn.

Work:

- Start from the model and `Sim` with the echo on the neighbour's FIFO
  and a sound drain
  ([the echo fix](/quest/m2/cluster-routing/routes/echo-order.md)).
- Give the model at least two prefixes and edits in flight: add, remove,
  widen, narrow, and an edit that crosses a claim change, a route change, a
  ROUTE_END, and its echo. The model fixes interest at the root today, so
  this is new state; keep it tractable by bounding edits as it bounds flaps.
- In the router, `Peer::asked` and `Peer::watch` become sets with a floor and
  a synced flag per prefix, `batch` takes the prefix its floor names, and a
  neighbour is one key for both directions, as `Sim` already has it. The
  test-only `watch` and interest generation go: an interest change is an
  edit, and a batch in flight before the marker applies to the old set
  instead of being discarded.
- Redo the handoff fixtures that an interest change touches
  (`widening_interest_needs_floors_scoped_to_what_they_cover`,
  `cancelling_interest_retires_its_region`,
  `widening_hides_what_was_frozen_until_a_covering_floor_arrives`,
  `a_snapshot_queued_before_widening_keeps_the_frozen_rest_hidden`, and the
  two scoped-grant widening fixtures) and both sweeps over edited sets.
- Update the `verify-route` skill's coverage table, its "Not covered" list,
  and its sync commit.

The session layer keeps one prefix per neighbour until the route stream
lands, so this changes no behaviour on its own.

Public API: none (the router is crate-private). Wire: none.

## Required

- [A node forgets a route only once nothing derived from it remains](/quest/m2/cluster-routing/routes/echo-order.md) - the model, `Sim`, and router with the echo on the neighbour's FIFO, per-link costs, and a sound drain, which this builds on

## Related

- [The route model covers a mesh with clients and an announce budget](/quest/m1/route-mesh-model.md) - grows the same spec to larger meshes and unequal costs; whichever lands second adapts
