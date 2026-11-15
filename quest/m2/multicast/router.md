# [XL] The new route rule runs as a sans-IO router

## Goal

`rs/moq-net/src/route/` holds a production router for the
[epic](/quest/m2/multicast/README.md)'s rule beside `path::Router`, sans-IO
and driven only by tests until the [switch](/quest/m2/multicast/switch.md).
The [bake-off](/quest/m2/multicast/bakeoff.md)'s fixtures and sweeps run on
it, the exhaustive delivery-order search checks it, and a benchmark swept
over forwarding nodes, publishers, broadcasts, and sessions shows that an
event costs work in what it changed.

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): the router first with no wire
change, then the switch, the order
[Routes and announces](/quest/m2/cluster-routes/README.md) Plan item 28 used
for the path rule.

- Build the rule as the bake-off's pick leaves it: routes to forwarding nodes
  only, the dead-hop withdrawal with no moved list, no hold, no doubt, no
  timer, claims ordered by revision and certified by floors, the chain
  across a scoped grant, and the one mark.
- Keep what is independent of the rule: the interest set edited in place
  ([moq-dev/moq#8728](https://github.com/moq-dev/moq/pull/8728)), and
  `Reader` and `View`.
- The reader debounces unavailable to offline locally, with mocked time, for
  as long as the bake-off measured routes take to settle. It replaces what a
  hold protected and is never on the wire.
- A relay stays on its next hop among routes of equal rank, and never sends
  a route back over the session it learned it on. Both were quests of their
  own until 2026-11-15: a change of hops alone was 3.5% to 11% of route
  messages, one link coming up on a 32-relay full mesh cost 592 route
  messages against 251, and advertising a route back to its source doubled
  route traffic between every pair of relays.
- Per-event work follows what changed, never every answering stream or every
  claim behind a route. [Route fan-out](/quest/m2/cluster-routes/route-fanout.md)
  pins where the path router fails this.
- Bound what a peer can make the router hold by its own requests, not by a
  constant.
- The exhaustive search over every delivery order
  ([moq-dev/moq#8729](https://github.com/moq-dev/moq/pull/8729)) moves to
  this router, and each property it checks is shown to fail on a mutation
  that breaks it. The maintainer chose on 2026-11-14 to keep that search over
  the compiled code and retire the TLA+ route model.

Public API: none. Wire: none.

## Required

- [The bake-off](/quest/m2/multicast/bakeoff.md) - the pick, and the fixtures this promotes
