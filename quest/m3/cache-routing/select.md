# [XL] Relays pull each broadcast from the cheapest copy

## Goal

A relay with demand for an Instance advertises it on the announce stream to
each peer or upstream that could take it, an edge asks its other upstreams
when it gains demand, and each relay pulls every demanded broadcast from the
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
maintainer's confirmation at merge: **adverts between peers and toward
upstreams, only where the copy could be taken; never down a link the
neighbour marked upstream, where the edge instead asks its other upstreams on
subscribe; the data path in every offer, the relays a pull feeds in every
subscription, and the race left standing**. Rejected:

- Advertising to every neighbour that could take the copy, down marked links
  too: it grows with a region's edges, since a warm core tells every edge it
  could feed. 1.7 to 118 warm messages per one-viewer reader change as edges
  per core grow from 3 to 128, against the pick's flat 1.6 to 1.8.
- Advertising to every neighbour, which also grows with the neighbours of
  every relay on the warm chain: 9 to 66 per one-viewer reader change as a
  full core mesh grows from 4 to 32 cores.
- Keeping adverts off the edges without the ask (never to a neighbour whose
  route runs through the relay, never down a marked link, or asking only
  between upstreams that tie): flat, but an edge whose cheaper uplink is to
  the cold core never hears of the warm one, 29 against 24 on the unequal
  region.
- Asking the k cheapest neighbours on each SUBSCRIBE. It stays bounded per
  subscribe only because it stops finding the copy: on a full drone mesh
  past 5 drones, `ask_1` shared no uplink at 64 drones and `ask_2` few. It
  doubled the time to a first frame and left about twice the readers unfed
  under churn. On CDN tiers `ask_1` sent 5.7 to 7 warm messages per
  one-viewer reader change against the pick's 1.6 to 2.1.
- Advertising only once two others want the copy, which lost both cases;
  never advertising over a link the sender marked upstream, which saves
  nothing in a core mesh.
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
- **Adverts.** While R has demand for an Instance besides N, and N has not
  marked R upstream, it tells N its offer and path on the announce stream
  when `O(R→N) + cold(R, N)` is below N's route cost for the Instance, or
  equals it through a next hop other than R, since a tie prefers a warm copy.
  It tells again on each change, and withdraws when the demand, its upstream,
  or that test ends. Nothing it skips between peers could ever be taken: a
  relay at rest costs no more than its own route, and a tie through R is the
  path join already. R knows N's route cost from what N announces it, and
  computes it when N's route runs through R. Skipping the parent and children
  by rule instead left 8 of 400 churned drone meshes unsettled.
- **Asks.** A relay with more than one upstream asks each upstream besides
  the one its path join takes, on a metadata-only subscription opened when
  it gains demand and closed when the demand ends. It does not wait for the
  answers: it joins along its route at once and moves if an answer is
  strictly cheaper. The asked upstream answers its offer and path, and each
  change. An upstream cannot tell which of its upstreams a dual-homed edge's
  route uses, since the edge announces it nothing, so only the edge can find
  a warm core beside its route.
- **The pull's chain.** Every subscription names the relays whose demand
  alone it carries: the subscriber, then, while the subscriber has no reader
  and one neighbour pulling through it, that neighbour's chain. It updates
  the chain on each change. A relay's demand besides N leaves out every pull
  whose chain names N. Without it N's own pull, through another neighbour,
  makes R's offer to N warm; N moves to R, the other pull ends, R's offer
  turns cold, and N moves back for good. One of 400 churned regions never
  settled for that reason, under the pick and most other warm arms.
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
Gaining demand lowers offers only to neighbours outside the chain that
brought it; losing it raises only the offer to the neighbour that still
pulls, which then moves only to something strictly cheaper. The sweep agrees:
every churned random mesh, drone mesh, and region settled, and cycles lasted
at most 38 deliveries under the pick, each while some answer was in flight.

### Simultaneous starts

Two drones reaching the CDN at once each pay their own warm uplink, so
neither offers the other anything cheaper, and both uplinks stay until one
drone's demand ends. The maintainer's rule, decided 2026-11-15: merge them
on the rendezvous hash unless that adds a cycle, an unfed reader, or more
messages than the second uplink costs. Under adverts it does add cycles:
in 400 churned drone meshes, 20 deliveries had a pull the merge made inside
a cycle, all transient. The one traced was a pending cycle: a merged
drone's new parent lost its uplink and rejoined through a drone that had
trusted the merged drone's advert, before the lost parent's answer arrived.
The merge also added about 1% unfed reader hops. Under asking on subscribe,
no merged pull was ever in a cycle.

So the race stands. A drone that loses the one uplink leaves every drone
it fed taking its own uplink at once: 4 uplinks on the five-drone shape,
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
  broadcast. An upstream holds at most one ask per downstream per demanded
  broadcast, and a pull's chain is no longer than the hop limit.
- Check the Goal's efficiency against plain path routing on the same shapes.
  On the simulator: pops 61 against 69 (cheapest tree 59), drones in turn one
  uplink against five, a ring of 48 relays 8 uplinks against 48, tiers 1095
  against 1191 (the cheapest), and an edge beside the warm core 24 against
  29, at 5 to 16.5 messages per reader change against 2 to 5.4.
- Port the growth sweep, and hold its laws, per link kind. Per reader change,
  warm messages are flat in the number of demanded broadcasts for every arm.
  - Between cores, a full peer mesh: flat in cores. A core with demand is
    never cheaper for another core than that core's own route.
  - Down a marked link: none. Up it: one ask per upstream beyond the first,
    with its answer, flat in edges per core. Together 1.6 to 2.1 per
    one-viewer reader change and 3.7 to 4.2 for a crowd, from 4 to 32 cores
    and 3 to 128 edges per core, up to 2,080 relays.
  - Between edges or drones that peer: n − 1 per first viewer (63 at 64
    drones), since every one could take the copy; that is what buys the
    cheapest tree there (432 against 1,328 to 1,456 for asking and path
    joins, a crowd of 9 on 16 broadcasts).
  At rest the busiest relay of 80 holds 518 adverts and asks for 4,096
  one-viewer broadcasts, against 4,355 metadata-only subscriptions for
  `ask_1` and 139,264 adverts sent to every neighbour.
- A region's edges do not peer every pair as it grows: they reach each other
  through the region's cores. Peered, the region is a drone mesh and pays its
  n − 1 per first viewer (65 at 64 edges per core); the tree it buys there,
  288 against 544 unpeered for a crowd, comes from pricing an edge-to-edge
  link at a third of an uplink.
- Port the cases as acceptance tests, two of them new: drones starting at
  once, whose uplinks the race keeps, and the same sweeps with each link
  delivering in order at its own pace, rather than one control message per
  delivery step across the mesh, which is what the simulator's unfed counts
  and first frames assume. A first frame matches the path join's when readers
  arrive in turn; when two edges start at once it took 262 ms against 150.
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
the announce stream carrying an Instance's offer and path; the same offer and
path on the subscribe answer and its updates, and on the answer to a
metadata-only subscription; and the chain of relays a subscription feeds, on
the subscribe and an update. Nothing lite-07 freezes changes.

## Required

- [The switch](/quest/m2/multicast/switch.md) - the route layer it ranks on

## Related

- [Route health](/quest/m3/route-health/README.md) - its metadata-only subscription answers the same way
