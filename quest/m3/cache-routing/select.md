# [XL] Relays pull each broadcast from the cheapest copy

## Goal

A relay with demand for an Instance advertises it on the announce stream to
each neighbour that could take it, and each relay pulls every demanded broadcast from the
cheapest copy of its Instance, moving an existing pull only when the route it
uses gets worse or ends, or another is strictly cheaper in summed cost. A peer
that sends no warm signal is treated as cold and behaves as it does today.
Relays export how many upstream copies they hold and how many moves a warm
copy caused.

## Plan

Rewritten 2026-11-15 (maintainer, /quest-plan on
[cache-aware multicast](/quest/m2/multicast/README.md)): the rule this was
to build, item 14 of the [epic](/quest/m3/cache-routing/README.md)'s earlier
Plan, is replaced.

Recommended 2026-11-16 on
[moq-dev/moq#8903](https://github.com/moq-dev/moq/pull/8903), awaiting the
maintainer's confirmation at merge: **advertise only to a neighbour that
could take the copy, with the data path in every offer, and the race left
standing**. Rejected:

- Advertising to every neighbour, which grows with the neighbours of every
  relay on the warm chain: 11 to 76 warm messages per one-viewer reader
  change as a full core mesh grows from 3 to 32 cores.
- Asking the k cheapest neighbours on each SUBSCRIBE. It stays bounded per
  subscribe only because it stops finding the copy: on a full drone mesh
  past 5 drones, `ask_1` shared no uplink at 64 drones and `ask_2` few. It
  doubled the time to a first frame and left about twice the readers unfed
  under churn. On CDN tiers `ask_1` sent 6.5 to 11 warm messages per
  one-viewer reader change against the pick's 3 to 7, about as many for a
  crowd (3.9 to 8.1 against 5.8 to 8), and built dearer crowd trees.
- Advertising only once two others want the copy, which lost both cases;
  never advertising over a link marked upstream, which saves nothing in a
  core mesh.
- Joins along the path only, the cost baseline, which delivers neither case.

That PR's simulator, `rs/moq-net/src/route/tests/warm.rs`, runs every rule
below, and its `warmth.rs` holds the shapes, the churn sweep, and the growth
sweep this ports onto production code.

### What a relay sends

- **The offer.** What a relay R with a live upstream through parent P offers
  neighbour N: `O(R→N) = O(P→R) + warm(P, R)` while R has demand besides N,
  otherwise `O(P→R) + cold(P, R)`, the chain priced unshared. The publisher's
  relay offers 0. An offer is never below its sender's own cost.
- **The path.** Every offer names the relays the data passes from the
  publisher's relay to the sender. Without it, transient cycles under churn
  rose by 73% on random meshes and 30% on drone meshes, and the merge below
  cannot work.
- **Adverts.** While R has demand for an Instance besides N, it tells N its
  offer and path on the announce stream when `O(R→N) + cold(R, N)` is below
  N's route cost for the Instance, or equals it through a next hop other than
  R, since a tie prefers a warm copy. It tells again on each change, and
  withdraws when the demand, its upstream, or that test ends. Nothing it
  skips could ever be taken: a relay at rest costs no more than its own
  route, and a tie through R is the path join already. R knows N's route cost
  from what N announces it, and computes it when N's route runs through R.
  Skipping the parent and children by rule instead left 8 of 400 churned
  drone meshes unsettled.
- **Answers.** The subscribe answer carries the parent's offer and path to the
  subscriber, and an update on each change. A subscriber waits for that answer
  before it counts the pull as live.

### What a relay chooses

- Its own cost is `D(R) = O(P→R) + warm(P, R)`. A candidate costs its offer
  plus the link's cold price, and the route costs the route layer's summed
  cold cost, so a route never ranks better for being extended.
- It pulls from a neighbour only when that is strictly cheaper than `D(R)`.
  It never pulls from a neighbour that pulls through it, nor from one whose
  path names it.
- A move is make before break: the old pull stays until the new one is
  answered live, and a move under way stays while it is no dearer than
  staying. Its own subscription shares the old parent, which lowers what
  staying costs, and dropping the move for that raises it again: a random
  mesh oscillated for good until this rule.
- A pending pull whose source left its options fails closed. Two relays that
  asked each other at once on stale adverts otherwise wait on each other for
  good.
- Equal candidates fall to a warm copy before the route, then the rendezvous
  hash, as `route_order` in `rs/moq-net/src/model/origin.rs` already ranks
  equal routes.

Why no cycle outlasts the messages in flight: a relay's cost is its parent's
offer plus a warm price of at least 1, and no offer is below its sender's
cost, so cost rises strictly along every pull chain. Two relays can never
each prefer the other on current offers, and a cycle needs one stale offer.
Gaining demand only lowers offers to others; losing it raises only the offer
to the neighbour that still pulls, which then moves only to something
strictly cheaper. The sweep agrees: every churned mesh settled, and cycles
lasted at most 57 deliveries, each while some answer was in flight.

### Simultaneous starts

Two drones reaching the CDN at once each pay their own warm uplink, so
neither offers the other anything cheaper, and both uplinks stay until one
drone's demand ends. The maintainer's rule, decided 2026-11-15: merge them
on the rendezvous hash unless that adds a cycle, an unfed reader, or more
messages than the second uplink costs. Under adverts it does add cycles:
in 400 churned drone meshes, 145 deliveries had a pull the merge made inside
a cycle (8 when advertising to every neighbour), all transient. The one
traced was a pending cycle: a merged drone's new parent lost its uplink and
rejoined through a drone that had trusted the merged drone's advert, before
the lost parent's answer arrived. The merge also added about 2% unfed reader hops. Under asking on
subscribe, no merged pull was ever in a cycle.

So the race stands. A drone that loses the one uplink leaves every drone
it fed taking its own uplink at once: 4 uplinks on the four-drone shape,
1 with the merge. If the maintainer takes the merge instead, it is: among
equal-cost copies a drone can reach, it joins the neighbour with the higher
rendezvous hash whose offer equals its own cost, once that leaves it no
dearer than its unshared cost and every other option. It never joins a
sibling under a parent other than the publisher's relay: leaving that parent
raises the parent's offer to the sibling the merge relied on. It needs the
path in each offer.

### What holds

- A warm link costs its warm price and a cold one its cold price, both at
  least 1, summed from the source along the path the data takes.
- Extend today's selection rather than adding a second selector. A front
  already moves to a better route of its own Instance and resplices
  (`rs/moq-net/src/model/front.rs`). A front moves only to a copy of its own
  Instance.
- The rule runs in production code that the fixtures and sweeps drive. The
  stripped rule lived only in a test simulator that checked it against
  itself, so a rule that never offered a copy passed. Port the warmth shapes
  and the churn sweep onto production relays: pops, drones in turn and at
  once, the lost uplink, the ring, and the tiers.
- Bound the warm state a peer can make a relay hold: one advert per
  neighbour per Instance the relay has a route for, a path no longer than the
  route layer's hop limit, and adverts for anything else dropped. Adverts are
  per broadcast like the claims the announce stream already carries over
  every link, so a relay hears at most one per neighbour per demanded
  broadcast.
- Check the Goal's efficiency against plain path routing on the same shapes.
  On the simulator: pops 61 against 69 (cheapest tree 59), drones in turn one
  uplink against five, a ring of 48 relays 8 uplinks against 48, tiers 1115
  against 1191 (cheapest 1095), at 5 to 9 messages per reader change against
  2 to 3.
- Port the growth sweep, and hold its laws. Per reader change, warm messages
  are flat in the number of demanded broadcasts for every arm. On CDN tiers
  with a full core mesh they are also flat in cores: 3 to 8 from 12 to 128
  relays and 1 to 4,096 broadcasts. A core with demand is never cheaper
  for another core than that core's own route, so only the edges beside it
  hear. On a full drone mesh they grow with the drones, n − 1 per first
  viewer (63 at 64 drones), since every drone could take the copy; that is
  what buys the cheapest tree there (432 against 1,328 to 1,456 for asking
  and path joins, a crowd of 9 on 16 broadcasts). At rest the busiest relay
  of 128 holds 755 adverts for 4,096 one-viewer broadcasts, against 5,405
  metadata-only subscriptions for `ask_1` and 151,552 adverts sent to every
  neighbour.
- A far pop keeps pulling through a nearer pop after that pop's reader
  leaves, while the nearer pop's unshared offer plus the warm link stays
  below the far pop's own long link. That is the cheaper tree, and the
  hysteresis that keeps it from flapping.
- Export upstream copies held, moves by cause (strictly cheaper, route lost
  or worse, parent lost its upstream), and adverts sent and held, through the
  relay's existing metrics.
- `rs/moq-net/AGENTS.md` says a relay behaves the same warm or cold. That
  covers answers to requests; this changes which neighbour a request goes
  to. Bring a rewording to the maintainer if the two contradict.

Decided 2026-11-14 (maintainer, pick on
[moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ lite-07
freezes without a warm wire, so whatever wire the pick needs lands in the
then-current wip version with this.

Public API: report any relay configuration added, such as a warm cost per
link. Wire, in the then-current wip version and all additive: an advert on
the announce stream carrying an Instance's offer and path, and the same
offer and path on the subscribe answer and its updates. Nothing lite-07
freezes changes.

## Required

- [The switch](/quest/m2/multicast/switch.md) - the route layer it ranks on

## Related

- [Route health](/quest/m3/route-health/README.md) - its metadata-only subscription answers the same way
