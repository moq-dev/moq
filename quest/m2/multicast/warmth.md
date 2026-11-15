# [L] Three ways to find a warm copy are measured

## Goal

The [bake-off](/quest/m2/multicast/bakeoff.md)'s harness runs three ways for
a relay to learn that a neighbour already has demand for a broadcast, over
the prototype route rule, and the PR reports for each: whether a CDN join
skips every pop without demand and whether drones with an uplink each settle
on one; the copies each link carries and the total link cost of each
broadcast's tree, against plain shortest-path joins and against the cheapest
tree; control messages per reader change; the time to a first frame; and
unfed readers and pull cycles under churn. The maintainer picks one, and the
PR rewrites [the implementation quest](/quest/m3/cache-routing/select.md)
for it.

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): ✅ measure all three, then
pick (rejected: picking one unmeasured). The warm wire was stripped from
lite-07 on 2026-11-14 because its benefit over plain path routing was never
measured.

Decided 2026-11-15 (maintainer, on
[moq-dev/moq#8873](https://github.com/moq-dev/moq/pull/8873);
[epic](/quest/m2/multicast/README.md) item 9):

- ✅ A node is warm for an Instance while it has demand for it: something
  downstream asked and its own upstream subscription is confirmed. Groups
  left in a cache do not make it warm (rejected: a yes/no cache state).
  Whether the signal is that fact alone or says how much demand stands
  behind it is for the measurement.
- ✅ Two cases must work, so the pick is the cheapest mechanism that
  delivers both:
  - **Pop skipping.** Pops joined by links that pass over other pops. A join
    for a broadcast no pop in between has demand for takes the long link and
    costs those pops nothing. Once one of them has demand, a farther pop
    joins through it when that is cheaper than the long link, and goes back
    when the demand ends.
  - **One uplink per group of drones.** Drones meshed with each other, each
    with a costly uplink to the CDN. The first with demand reaches the CDN,
    and the rest join it over the mesh, because a warm drone costs less than
    a cold uplink. Two that reach out at once end on one uplink, and when
    the drone holding it loses it, another takes over.

The three:

1. **On the path only.** A join stops at the first relay with demand for the
   Instance, and equal-cost next hops are chosen by the rendezvous hash, so
   siblings facing the same choice converge on one. No warm signal. A join
   never learns of demand off its own path, so this delivers neither case:
   it is the baseline for cost, not a candidate.
2. **Ask on subscribe.** Each SUBSCRIBE answer, payload or metadata-only,
   says whether the neighbour has demand for the Instance and its summed
   cost from the source, and the relay takes the cheapest. A
   [metadata-only subscription](/quest/m3/route-health/meta.md) doubles as
   the query, so nothing is sent for a broadcast nobody asks about. Sweep how
   many neighbours a relay asks.
3. **Advertise.** A relay with demand announces it on the announce stream.

The constraint the maintainer set for all three: a warm link costs its warm
price and a cold link its cold price, both at least 1, summed from the source
along the path the data takes, so a route never ranks better for being
extended. The stripped rule broke it: a carrier offered its copy at no cost
and a hash ranked carriers, and pulls formed a cycle in 3.3% of churn steps,
most leaving a reader unfed.

What that constraint gives and what it leaves: with each relay's cost summed
along the path its data takes, two relays can never each prefer the other,
since one would need `b + link < a` and the other `a + link < b`. For the
same reason two drones that reached out at once, each at the cost of its own
warm uplink, never merge. Merging them needs a rule both compute alike, such
as the lower rendezvous hash of Instance and Node ID keeping its uplink, and
a move that completes only once the new copy delivers. Show that rule adds
no cycle, or report that simultaneous starts stay on two uplinks and what
that costs.

Joining the cheapest point of the tree that exists is the greedy online
Steiner tree, which stays within a logarithmic factor of the cheapest tree
when readers only join (Imase and Waxman, 1991). Leaves and moves are what
the sweep adds to that.

A cost that depends on who subscribed is outside the
[proof](/quest/m2/multicast/proof.md)'s theorem. Argue it separately and
sweep it: taking a route only makes it cheaper for others, and leaving one
only worsens an offer nobody uses. A cycle that outlasts the messages in
flight fails the mechanism; count the transient ones with the readers they
leave unfed.

Shapes where the three differ: the two cases above; two edges that each
prefer a different core; a ring with a far source; and one viewer per
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
