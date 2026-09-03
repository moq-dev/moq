# [L] Rank

## Goal

A relay adopts another relay's warm copy only when that relay is strictly
closer to the publisher, so a PoP converges on one aggregation point instead of
a coin flip, and no two relays can adopt each other.

## Plan

Once [Warm advertise](/quest/m2/pop-skipping/warm-advertise.md) lands, two
relays carrying one broadcast both advertise it warm and tie: warm cost cannot
separate them, and only the deterministic hash would, so the aggregation root is
picked at random. In the `sjc0 -> dal0 -> iad0` topology that lets SJC (two
links from IAD) win over DAL (one link), and the cluster carries the extra
backhaul it was supposed to remove.

Break that tie on cold cost, which is the relay's own distance to the publisher
with warm discounts removed and is already on the wire and already ranked below
warm in `route_order`. Adoption descends `(cold cost, hash(broadcast path, relay
hop id))`: lower cold wins, equal cold takes the lower hash, and a relay keeps
its own upstream rather than adopting a peer that ranks above it. Including the
broadcast path in the hash spreads ownership instead of making one relay win
every broadcast. A relay advertises its *own* rank, not that of a parent it
adopted, so every warm edge descends and cycles cannot form.

Adopting a parent adds that link to the adopter's cold cost, so it can only rank
above its parent afterwards. That is what makes descent automatic, and it is why
no link may be priced free.

### The hold, and why it is not optional

Cold is a value each relay reports about itself, so a report still crossing the
mesh can be lower than what its sender would say now. Rings of relays can each
rank a stale neighbour below themselves and all let go at once, leaving the
broadcast with no source. Rising costs are the whole hazard; if costs only fell,
a stale value would only make a peer look worse than it is. A GOAWAY prices a
route at the ceiling while neighbours still remember it cheap, so the trigger is
a rolling restart, not an exotic race.

Hold a re-parent onto another relay long enough for the costs it rests on to
land, and re-evaluate when the hold expires rather than committing to the
decision that armed it. The sizing rule is "longer than an announcement crosses
the mesh", plus a stable per-relay spread so a PoP does not reconsider on one
instant. The hold covers only trading a working upstream for a better one:
an idle relay is pulling nothing, a one-hop chain is the publisher itself, and
leaving a drained or vanished route stays immediate.

### Two identities, not one

Adoption is a statement about the adjacent relay, and that is a different
identity from the first-hop resume rule [moq#3312](https://github.com/moq-dev/moq/pull/3312) landed.
The pre-#3225 front kept both, because they answer different questions:

- The **first** hop is who produced the content. Alternate routes to one
  publisher deliberately share it, which is what makes them spliceable, and it
  is what the resume rule keys on.
- The **last** hop is the peer that advertised the route: the parent a relay
  would be adopting. `handover_allowed` and the hold both keyed on it, and the
  rank hash was taken over it (`fnv_key(name, [peer])`).

Use the last hop here. Keying adoption on the first hop would make every carrier
of one broadcast look like the same peer, so a changed parent would go
undetected and two relays could adopt each other, which is the failure the hold
exists to prevent.

What this quest reuses from the resume rule is the comparison rather than the
field: `Hop::UNKNOWN` identifies nobody and never matches itself, so two
anonymous relays must not pass for one relay reconnecting and skip the gate.

Update `drafts/draft-lcurley-moq-lite.md` in the same change, restoring the
adoption-rank rule that
[moq#3278](https://github.com/moq-dev/moq/pull/3278) removed.

### Tests

The asymmetric chain (DAL outranks SJC regardless of hash), the equal-cost race
(exactly the lower hash keeps its upstream while the other adopts it), a
three-node transitive tree, simultaneous updates, a ring of relays each holding
a stale cheaper report (which must not leave the broadcast sourceless), route
loss and reversion, and an unknown-cold peer losing to a known cheap one in both
hash directions. A live track must migrate only at a group boundary and must not
see announcement churn.

Write every gate test over both hash directions, or it proves nothing beyond a
lucky hash.

## Required

- [Warm advertise](/quest/m2/pop-skipping/warm-advertise.md) - there is nothing
  to rank until two relays can both advertise one broadcast as warm
