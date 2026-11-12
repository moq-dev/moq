# [L] Prove cache-aware routing

## Goal

Sweeps over the route layer show that the [epic](/quest/m3/cache-routing/README.md)'s
warm routes and eligibility rule keep per-broadcast forwarding loop-free at
every step, under stale, reordered, and in-flight messages. Once demand and
topology stop changing, every demanded broadcast is delivered, parent choices
stop changing, and a relay carries a broadcast only when it has a local
reader, feeds two or more downstream sessions, or is its one consumer's
cheapest cold route, as the epic's Goal states. The warm message's shape and wire placement are chosen
and recorded in the epic's Plan.

## Plan

Decided 2026-11-09 (maintainer, /quest-plan from Kixel's audit of its fleet
plan): ✅ pull the proof into m2, ahead of the lite-07 cut, so a warm
message that ships in lite-07 is the proven one (rejected: a design-only
quest in m2 that sizes the message with no proof; leaving this in m3 and
taking an additive SETUP extension if it lands after the cut). This reverses
the 2026-11-08 choice of m3 for the proof alone;
[the implementation](/quest/m3/cache-routing/select.md) stays in m3. If the
proof places the message in lite-07, this quest also lands its draft section
and its codec in Rust and JS, parsed and ignored until m3, so the cut carries
it; a SETUP extension needs neither before the cut, and
[Finalize moq-lite-07](/quest/m2/lite07-finalize.md) then drops this quest
from its list. Either placement is now this quest's to record, with no
second trip to the maintainer for choosing lite-07.

Reopened 2026-11-11 (maintainer, /quest-plan on the route redesign, epic
Plan item 8): a warm route propagates, and the rule that keeps it free of
loops and stale copies is
[the prototype](/quest/m1/route-stale-copies.md)'s to pick.
The eligibility rule and the fixtures below are for the one-hop warm route
first decided, which [moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457)
proves. Once the prototype's pick is recorded, rewrite both for it: the
fixtures keep their meshes, and gain a relay that joins a copy at a neighbour
farther from the publisher than itself.

Build on the sans-IO `Router` in `rs/moq-net/src/route/`, which each
origin's `route::Node` drives, and the harness behind its handoff sweeps,
not the reference model in
`rs/moq-net/tests/integration/routing_contracts/`, which
[Route layer acceptance](/quest/m2/cluster-routing/routes/acceptance.md)
deletes. The router exposes one selected route per origin; the proof needs
each origin's eligible neighbors as well, so add that as a crate-private
query. Add per-broadcast demand and forwarding on top of the router state:
readers joining and leaving, each relay's parent per broadcast, warm routes
per neighbor, and the existing linger.

Fixtures, each a deterministic counterexample the rules must survive:

- A chain of three relays plus a skip link, priced 3 and 5: cold skip, warm
  join, and the return to the skip when the skipped relay's reader leaves.
- A relay whose only consumer is N withdraws its warm route toward N. N
  bypasses it over a cheaper skip link, and stays when the relay is N's
  cheapest cold route.
- Two relays at equal cold metric, both warm: only the lower hash keeps its
  upstream, the other joins it, and they never adopt each other.
- A ring of relays each holding a stale, lower report of a neighbor's cold
  metric, the hazard behind the pre-#3225 hold: no loop and no broadcast left
  without a source.
- A warm withdrawal in flight while N's request is on its way: the former
  carrier forwards the request along its own route, since it answers the same
  warm or cold, and forwarding stays loop-free.
- An origin seqno advance, a route loss, and a relay that drains (GOAWAY
  makes a neighbor least preferred) while broadcasts are being pulled.
- A warm route for another instance of the same path is never joined.
- k LAN relays with their own uplinks: one uplink copy per broadcast in a
  full mesh; record the copies a sparse ring keeps, which the epic accepts.

Sweeps churn random meshes: readers come and go, sessions drop and return,
and messages are held and delivered out of step. Check at every step that the
per-broadcast forwarding graph has no cycle, and at quiescence the Goal's
steady state. Report warm messages per demand change and confirm an idle
broadcast sends none.

Wire: design the warm message, which names at least the path, the origin's
Route ID, and the instance, and say which stream carries it and how a peer
that never sends one behaves. Size it against lite-07 and an additive SETUP
extension and record the choice and its reason in the epic's Plan.

## Required

- [A prototype settles what keeps a route free of stale copies](/quest/m1/route-stale-copies.md) - the rule a propagating warm route rests on, which replaces the one this proved

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the feasibility rules and handoff sweeps this extends
