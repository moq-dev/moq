# [L] A dual-homed publisher stays reachable while one link flaps

## Goal

A publisher connected to two relays whose link to one of them closes and
reopens faster than the doubt timer stays reachable at every relay through
its other link, the whole time. The path rule's doubt still keeps relays
off routes through a dead relay until its timer fires.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on the BGP failure modes): ✅ fix
it, checked in TLA+ as well as on real sessions (rejected: pinning today's
behaviour as an accepted cost).

The bug, as the path rule stands ([epic](/quest/m2/cluster-routes/README.md)
Plan item 27): a closed session casts doubt on the node at its far end, and
every relay that hears the DEAD doubts that node. When the closed session is
the publisher's own, the doubted node is the publisher, and every route to
its claims names it, the route through its healthy link included. A relay
that was using the closed link cannot move to the healthy one, and each flap
opens a new session whose close extends the doubt. While the link keeps
flapping faster than the timer, those relays have no route although the
publisher was never unreachable. [moq-dev/moq#8622](https://github.com/moq-dev/moq/pull/8622)
measured the general cost: 2.87× more reachable-but-unrouted observations
under timer-only doubt.

✅ Model first: doubt blocks a route that passes through a node, not one that
ends at it as its origin (rejected: capping a doubt at one timer from the
first close, which still leaves a gap after each flap; modeling both). Doubt
exists because a dead relay is reported one neighbour at a time, so a relay
could move onto a stale route through it and loop, and a route cannot loop
through its own endpoint. The model has to show what the rule gives up: when
the origin really dies, a relay may move onto a stale route to it that a
neighbour's DEAD has not yet withdrawn. Check that such a route is still
chased out (`StaleIsChased`) and that requests stay loop-free
(`RequestsLoopFree`). Decide in the PR what counts as the origin when a
relay originates a route for a broadcast it carries
([cache-aware routing](/quest/m3/cache-routing/README.md)).

`fresh-publisher.md`, added by #8622, exempts only a publisher's own new
direct session from doubt, so it never helps a relay further away. If this
rule holds, it covers that case too: fold that quest into this one or delete
it, whichever has landed first.

Work:

- `rs/moq-net/verify/route/RouteHandoff.tla`: a config with a publisher on
  two relays and flaps on one link, and a property that a relay with a path
  of sessions that never closed to the publisher routes to it once that path
  is known. Run the existing configs under the new rule.
- The path router (`route::path`, from
  [the router](/quest/m2/cluster-routes/scope.md)) applies the rule, with the
  model's counterexamples reproduced there first.
- A fixture on real sessions with mocked time: the dual-homed publisher with
  one link flapping at half the timer, where a subscriber at a far relay
  receives every group while it flaps.
- Re-run #8622's reconnect and churn measurements under the new rule and
  report reachable-but-unrouted observations, stale routes, and loops beside
  timer-only doubt.
- Update the epic's Plan item 27, the draft's Routing section if
  [the switch](/quest/m2/cluster-routes/switch.md) has landed, and the
  `verify-route` skill.

## Required

- [The router runs the path rule with a route per scope](/quest/m2/cluster-routes/scope.md) - the doubt this changes

## Related

- [A relay that refuses a forwarded route drops the route, not the session](/quest/m1/refuse-route.md) - another failure mode from the same BGP comparison
- [A request loop heals once routes settle](/quest/m2/cluster-routes/request-loop.md) - another failure mode from the same BGP comparison
