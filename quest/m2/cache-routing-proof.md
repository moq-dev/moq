# [L] Prove cache-aware routing

## Goal

Sweeps over the route layer show that the [epic](/quest/m3/cache-routing/README.md)'s
warm routes, which propagate per broadcast, stay loop-free at every step,
under stale, reordered, and in-flight messages. Once demand and topology stop
changing, every demanded broadcast is delivered, parent choices stop
changing, and a relay carries a broadcast only when it has a local reader,
feeds two or more downstream sessions, or is its consumers' cheapest route
given the copies the others hold, as the epic's Goal states. Lite-07's warm
message carries the proven rule.

## Plan

Decided 2026-11-09 (maintainer, /quest-plan from Kixel's audit of its fleet
plan): ✅ pull the proof into m2, ahead of the lite-07 cut, so a warm
message that ships in lite-07 is the proven one (rejected: a design-only
quest in m2 that sizes the message with no proof; leaving this in m3 and
taking an additive SETUP extension if it lands after the cut). This reverses
the 2026-11-08 choice of m3 for the proof alone;
[the implementation](/quest/m3/cache-routing/select.md) stays in m3.

Reopened 2026-11-11 (maintainer, /quest-plan on the route redesign, epic
Plan item 13): a warm route propagates, and the rule that keeps it free of
loops and stale copies is the route's path
([routes epic](/quest/m2/cluster-routes/README.md) Plan item 27,
picked 2026-11-11 on
[moq-dev/moq#8533](https://github.com/moq-dev/moq/pull/8533)), which also
removes the seqno, ROUTE_REQUEST, and the ROUTE_END echo.

[moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457) proved the
one-hop warm route first decided (epic items 8 to 12):
`rs/moq-net/src/route/tests/cache.rs` models demand, parents, warm routes per
neighbor, linger, drain, and legacy peers over the real `Router`, through the
crate-private `Router::eligible`, and lite-07 carries its wire (WARM_START,
WARM_UPDATE, WARM_END, and `Serves` on SUBSCRIBE), which Rust reads and
ignores. Rewrite the model, its fixtures, and that wire for the pick. Start
from `rs/moq-net/src/route/tests/copies.rs`, which runs a relay joining a
copy at a neighbour farther from the publisher than itself, two relays that
start carrying together, and a copy that goes, on the prototype router. It
ranks carriers by a hash of the broadcast and the carrier; the proof settles
whether that rank is the rule.

The fixtures in `cache.rs` keep their meshes:

- A chain of three relays plus a skip link, priced 3 and 5: cold skip, warm
  join, and the return to the skip when the skipped relay's reader leaves.
- A relay whose only consumer is N withdraws its warm route toward N.
- Two relays at equal cost, both warm: the rank picks one carrier, and they
  never adopt each other.
- A ring of relays each holding a stale report of a neighbor's cost: no loop
  and no broadcast left without a source.
- A warm withdrawal in flight while N's request is on its way.
- A route loss, and a relay that drains (GOAWAY makes a neighbor least
  preferred) while broadcasts are being pulled.
- A warm route for another instance of the same path is never joined.
- k LAN relays with their own uplinks: one uplink copy per broadcast in a
  full mesh; record the copies a sparse ring keeps.

Add a relay that joins a copy at a neighbour farther from the publisher than
itself. Make every sweep scenario settle: `UNSETTLED` in `cache.rs` names
scenario 2251, which never does under the one-hop rule, and the sweep's long
run (`sweep_cache_routing_long`) leaves the merge gate's ignore list for
nightly once it is empty. Report warm messages per demand change and confirm
an idle broadcast sends none. Update the draft's Warm Routes section and the
Rust and `js/net` codecs in the same PR.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the path rule this extends to warm routes
