# [XL] The new route rule runs as a sans-IO router

## Goal

`rs/moq-net/src/route/` holds a production router for the
[epic](/quest/m2/multicast/README.md)'s rule beside `path::Router`, sans-IO
and driven only by tests until the [switch](/quest/m2/multicast/switch.md).
The [bake-off](https://github.com/moq-dev/moq/pull/8880)'s fixtures and sweeps run on
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

Measured 2026-11-15 by the [bake-off](https://github.com/moq-dev/moq/pull/8880),
whose prototype is `rs/moq-net/src/route/tests/multi.rs`; its recommendations
stand until the maintainer picks otherwise on that PR:

- The rule as written hunts: 6.6 million route messages for a partition of a
  32-relay CDN, against 579 today. Build these three with it, each an arm of
  the bake-off: the highest session known closed per pair of nodes, kept for
  good and refusing any path at or below it (`floor`, a departure from the
  epic's "no dead hop remembered"); advertising once per burst of input read
  rather than after every message (`batch`); and a node that sent GOAWAY named
  once per neighbour as a closed session is (`drain`).
- A closed session's message ends, on that stream, every route its sender
  offered through the session; the sender owes each one its next offer or an
  explicit end. A neighbour's copy of the claims stays through that, so a
  reroute sends no claim again.
- A claim's withdrawal says whether its publisher ended it or its announcer
  lost it, so a publisher's session closing shows readers offline and never a
  clean end. A claim carries the lowest link priority to its publisher, or a
  node prefers its own echo of a claim that crossed two grants and never
  settles.
- Not picked, with their costs on the PR: holding adverts until neighbours
  agree (`quiet`, which can wait for good on a neighbour that learned a stale
  route late), ranking the far end of a closed session as draining
  (`suspect`, which shortens a silent death but doubles the messages of a
  flap and has no word that always lifts it), and refusing a route a
  neighbour's own offer contradicts (`agree`, a small gain).
- Promote `bakeoff.rs`'s fixtures and `sweep_the_prototype_carries_every_failure_at_32_relays`.
  Size the reader's debounce from the PR's `fed_ms` column: 375 ms was the
  longest for a failure that left a path on 32 relays, a silent death aside.
- Selected routes looped for a moment in partitions and deaths (up to 3 rounds
  at 32 relays), which today's never do; requests must fail closed first.

Public API: none. Wire: none.
