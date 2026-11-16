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
whose prototype is `rs/moq-net/src/route/tests/multi.rs`, and decided
2026-11-16
([maintainer](https://github.com/moq-dev/moq/pull/8880#issuecomment-6911055514),
the [epic](/quest/m2/multicast/README.md)'s items 13 and 14):

- The rule as written hunts: 6.6 million route messages for a partition of a
  32-relay CDN, against 579 today. Build these five with it, each an arm of
  the bake-off: the highest session known closed per pair of nodes, kept for
  good and refusing any path at or below it (`floor`, a departure from the
  epic's "no dead hop remembered", and `epoch`, below, which bounds it);
  advertising once per burst of input read
  rather than after every message (`batch`); and a node that sent GOAWAY named
  once per neighbour as a closed session is (`drain`); and a node offering a
  route to itself only once it has a claim to announce, its endpoints' or one
  it announces again across a scoped grant, and from then on until it restarts
  (`claimed`). With all five that partition cost 690.
- `claimed` makes the route per announcing relay, not per forwarding node:
  held routes fell from 6,576 to 207 on 32 relays with one publisher, under
  today's 221. A publisher's first claim on a relay then starts a route at
  every node (201 route messages on that CDN), and across scoped grants a
  leaving claim's echo can make a gateway announce for the first time
  (`a_first_claim_starts_a_route`). Keep the route standing after the last
  claim leaves: withdrawing it races the claim's own end, which would show a
  reader offline where the publisher ended cleanly.
- A closed session's message ends, on that stream, every route its sender
  offered through the session; the sender owes each one its next offer or an
  explicit end. A neighbour's copy of the claims stays through that, so a
  reroute sends no claim again.
- A claim's withdrawal says whether its publisher ended it or its announcer
  lost it, so a publisher's session closing shows readers offline and never a
  clean end. A claim carries the lowest link priority to its publisher and
  whether the way there is draining, or a node prefers its own echo of a claim
  that crossed two grants and never settles: two relays sharing a draining
  neighbour and a scoped link traded places for good without the second
  (`a_claim_across_a_grant_keeps_its_drain`). A scoped neighbour's
  GOAWAY drains the claims it gave the same way
  (`a_scoped_neighbour_draining_drains_its_claims`), and a session that
  closes takes what was still unsent to it.
- A node that takes a newer copy of a forwarding node's claims passes the floor
  on even when nothing in the copy differs, or a neighbour that heard a claim
  in between keeps it after its publisher ended it
  (`a_newer_floor_travels_with_nothing_changed`).
- Bound the session floor: alone it keeps an entry per restart, since a
  restarted relay is a new node. Put the relay and a start that only grows in
  the node identity, file the floor per pair of relays, and order a session
  by its node's start and then its number (`epoch`): a restart then replaces
  the entry, and no message or time changed on any row
  (`a_session_floor_keeps_an_entry_per_restart_unless_filed_by_relay`). A
  start that repeats or goes back gets the relay's new sessions refused.
  The bound is then pairs of relay identities ever heard closed: fixed where
  relays are configured, as on a CDN, and growing by a departed device's
  pairs on a mesh whose devices mint identities. Measure that growth on the
  mesh shapes, and list any eviction it needs as a decision in this PR; one
  that waits on a clock is the maintainer's.
- Not picked, with their costs on the PR: holding adverts until neighbours
  agree (`quiet`, which can wait for good on a neighbour that learned a stale
  route late), ranking the far end of a closed session as draining
  (`suspect`, which shortens a silent death but doubles the messages of a
  flap and has no word that always lifts it), and refusing a route a
  neighbour's own offer contradicts (`agree`, a small gain).
- Promote `bakeoff.rs`'s fixtures and `sweep_the_prototype_carries_every_failure_at_32_relays`.
  Size the reader's debounce from the longest path a route may take times a
  hop's delay, not from a sample: the PR's `fed_ms` column is what was seen,
  775 ms at most for a failure that left a path on 32 relays (the
  publisher's relay dying on the full mesh), a silent death aside, and a
  longer path settles later.
- Selected routes looped for a moment in partitions and deaths (up to 3 rounds
  at 32 relays, where today's reached 4); requests must fail closed first.

Public API: none. Wire: none.
