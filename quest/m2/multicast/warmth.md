# [L] Three ways to find a warm copy are measured

## Goal

The [bake-off](/quest/m2/multicast/bakeoff.md)'s harness runs three ways for
a relay to learn that a neighbour already carries a broadcast, over the
prototype route rule, and the PR reports for each: the copies each link
carries and the total link cost of each broadcast's tree, against plain
shortest-path joins and against the cheapest tree; control messages per
reader change; the time to a first frame; and unfed readers and pull cycles
under churn. The maintainer picks one, and the PR rewrites
[the implementation quest](/quest/m3/cache-routing/select.md) for it.

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): ✅ measure all three, then
pick (rejected: picking one unmeasured). The warm wire was stripped from
lite-07 on 2026-11-14 because its benefit over plain path routing was never
measured.

The three:

1. **On the path only.** A join stops at the first relay carrying the
   Instance, and equal-cost next hops are chosen by the rendezvous hash, so
   siblings facing the same choice converge on one. No warm signal. This is
   the baseline the other two must beat.
2. **Ask on subscribe.** Each SUBSCRIBE answer, payload or metadata-only,
   says whether the neighbour carries the Instance and its summed cost from
   the source, and the relay takes the cheapest. The
   [metadata-only secondary](/quest/m3/route-health/README.md) doubles as
   the query, so nothing is sent for a broadcast nobody asks about. Sweep how
   many neighbours a relay asks.
3. **Advertise.** A carrying relay announces it on the announce stream.

The constraint the maintainer set for all three: a warm link costs its warm
price and a cold link its cold price, both at least 1, summed from the source
along the path the data takes, so a route never ranks better for being
extended. The stripped rule broke it: a carrier offered its copy at no cost
and a hash ranked carriers, and pulls formed a cycle in 3.3% of churn steps,
most leaving a reader unfed.

A cost that depends on who subscribed is outside the
[proof](/quest/m2/multicast/proof.md)'s theorem. Argue it separately and
sweep it: taking a route only makes it cheaper for others, and leaving one
only worsens an offer nobody uses. A cycle that outlasts the messages in
flight fails the mechanism; count the transient ones with the readers they
leave unfed.

Shapes where the three differ: two edges that each prefer a different core;
LAN relays with an uplink each; a ring with a far source; and one viewer per
broadcast, where the first must cost no message at all. Earlier sweeps to
compare with: a LAN mesh of 6 pulled one uplink copy per broadcast, a ring
of 6 kept 12 copies for 8 broadcasts, and warm routes cost 3.1 to 4.5
messages per reader change.

The cut waits on the pick, not on the implementation. An additive field
lands in whichever wip is current with its implementation, as decided
2026-11-14; the PR says whether the pick changes bytes lite-07 would freeze.

Public API: none. Wire: none.

## Required

- [The bake-off](/quest/m2/multicast/bakeoff.md) - the harness and the route rule the three run over
