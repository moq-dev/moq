# [XL] Relays pull each broadcast from the cheapest copy

## Goal

A relay with demand for an Instance advertises it on the announce stream to
each peer or upstream that could take it, an edge queries its other upstreams
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
upstreams, only where the copy could be taken, never down a link the neighbour
marked upstream, and never up one with a copy that came over one; an edge
instead queries its other upstreams once when it gains demand; the data path
in every offer; no warm signal taken from a relay on the taker's own path; and
the race left standing**. Four parts: adverts, the path, the query, and one
rule. Adverts reuse the route layer's mark: it passes no route from one marked
link to another, so an edge carries nothing between its cores.

The maintainer's decision is whether to keep the query, below. Rejected:

- Advertising to every neighbour that could take the copy, down marked links
  too: it grows with a region's edges, since a warm core tells every edge it
  could feed. 1.7 to 118 warm messages per one-viewer reader change as edges
  per core grow from 3 to 128, against the pick's flat 1.0.
- Advertising to every neighbour, which also grows with the neighbours of
  every relay on the warm chain: 9 to 66 per one-viewer reader change as a
  full core mesh grows from 4 to 32 cores.
- Asking only between upstreams that tie, or never advertising to a neighbour
  whose route runs through the relay: as the pick without the query, at more
  messages or none fewer.
- A standing query, held open for updates while the demand lasts: the same
  trees as the one-shot query, but the upstream holds an entry per edge per
  demanded broadcast (32 per core, against none) and it left 1 of 400
  churned regions unsettled.
- Naming in every subscription the relays whose demand alone it carries, so
  an offer leaves them out: a second fix for the loop the rule stops. It adds
  a list and an update message on each change (6,904 to 8,485 per 400
  churned meshes), loses a shared drone uplink (41 against 33 with two drones
  a hop apart), and left 1 of 2,000 churned random meshes unsettled, as the
  rule does, though a different one.
- Asking the k cheapest neighbours on each SUBSCRIBE. It stays bounded per
  subscribe only because it stops finding the copy: on a full drone mesh
  past 5 drones, `ask_1` shared no uplink at 64 drones and `ask_2` few. It
  doubled the time to a first frame and left about twice the readers unfed
  under churn. On CDN tiers `ask_1` sent 5.7 to 7 warm messages per
  one-viewer reader change against the pick's 1.0.
- Advertising only once two others want the copy, which lost both cases;
  never advertising over a link the sender marked upstream, which saves
  nothing in a core mesh.
- Joins along the path only, the cost baseline, which delivers neither case.

That PR's simulator, `rs/moq-net/src/route/tests/warm.rs`, runs every rule
below, and its `warmth.rs` holds the shapes, the churn sweep, and the growth
sweep this ports onto production code.

### Decision: the query

The query is the one part that only an edge with more than one upstream
uses. Without it the pick is adverts, the path, and the rule, and an unpeered
CDN sends no warm message at all. With equal uplinks the query buys nothing: every edge of
a region ranks the two cores by the same rendezvous hash per broadcast, so
every edge's path join lands on the same core, always; path joins already
built the cheapest tree on every unpeered CDN shape swept. It pays only where an
edge's cheaper uplink is to the core that is cold.

| shape | tree, with / without the query (cheapest) | warm messages per reader change, with / without |
|---|---|---|
| CDN, 8 cores, 16 edges per core, one viewer | 2236 / 2236 (2236) | 1.00 / 0.00 |
| CDN, 8 cores, 16 edges per core, crowd | 544 / 544 (544) | 1.00 / 0.00 |
| CDN, 32 cores, 3 edges per core, crowd | 256 / 256 (256) | 1.00 / 0.00 |
| tiers, crowd (edges peered) | 1095 / 1095 (1095) | 4.71 / 3.38 |
| tiers, one viewer | 1096 / 1096 (1096) | 5.50 / 3.50 |
| two edges, unequal uplinks, in turn | 12 / 13 (12) | 2.50 / 0.50 |
| two edges, unequal uplinks, at once | 9 / 9 (7) | 3.00 / 1.00 |
| one region, unequal uplinks | 24 / 29 (24) | 2.00 / 0.00 |
| 400 churned regions: settled trees over cheapest; unfed reader-hops | +4.9% / +5.3%; 22,477 / 16,810 | 21,182 / 4,879 in all |

Recommended: keep the query. Where uplinks are unequal, it saves the second
core's copy over the core mesh, the dearest link a region has. It costs one
request and one answer per extra upstream per first viewer at an edge, holds
nothing on the core, and carries no traffic. Under churn it is dearer: 4.3
times the warm messages and 34% more unfed reader-hops, much of which is the
simulator delivering one control message per step across the whole mesh, so
every query delays a route or a subscribe somewhere else. Without it an
operator gets the same tree by pricing each edge's uplinks equally.

### What each part costs

| part | on the wire | relay state | bounded by | without it |
|---|---|---|---|---|
| advert | offer and path on the announce stream, and its withdrawal | one heard per neighbour per demanded Instance | neighbours; the path by the hop limit | path joins: pops 69 against 61, drones in turn 5 uplinks against 1, a ring 48 against 8 |
| the route layer's mark on adverts | one bit on the advert, the answer, and the query's answer, as a route's offer carries | the bit with each heard advert and live pull | one per offer | an edge carries a copy between its cores, which no route would: 51 readers in 400 churned regions fed that way, and 1.05 to 1.55 warm messages per one-viewer reader change on an unpeered CDN against 1.0 |
| offer on the subscribe answer | a cost on the answer and its updates | one per pull | one per subscription | nothing can price a move |
| path in the offer | a list on the advert, answer, and query answer | as the offer | the hop limit | cycle deliveries up 72%, 7%, and 156% on random meshes, drones, and regions; 3 of 1,200 churned meshes unsettled |
| query | a request naming an Instance; one answer, offer and path or none | the edge keeps one answer per extra upstream per demanded Instance; the upstream none | upstreams beyond the first | an edge whose cheaper uplink is cold: 29 against 24 |
| no warm signal from a relay on the taker's path | none | none, the path of its own pull | none | 1 of 400 churned regions never settles. Its price: a move up the taker's own branch to a relay warm on its own account, 14 against 13 in `an_ancestor_warm_on_its_own_is_passed_over`; under churn the chain, which keeps that move, settled on trees about as dear (+2.6%, +9.6%, +5.2% over cheapest against the rule's +2.7%, +9.4%, +4.9%) |

The rule replaces a chain in every subscription naming the relays whose
demand alone it carries. The offer's path cannot stand in for the chain on
the sender's side: the demand that comes back is downstream of the sender,
and its path names only relays upstream of it. On the taker's side the path
of its own pull is enough.

### What a relay sends

- **The offer.** What a relay R with a live upstream through parent P offers
  neighbour N: `O(R→N) = O(P→R) + warm(P, R)` while R has demand besides N,
  otherwise `O(P→R) + cold(P, R)`, the chain priced unshared. The publisher's
  relay offers 0. An offer is never below its sender's own cost.
- **The path.** Every offer names the relays the data passes from the
  publisher's relay to the sender. Without it, cycle deliveries under churn
  rose by 72% on random meshes, 7% on drone meshes, and 156% on regions, 1
  drone mesh and 2 regions of 400 never settled, and the merge below cannot
  work.
- **Adverts.** While R has demand for an Instance besides N, N has not
  marked R upstream, and R has not both marked N upstream and taken its copy
  over a marked link, it tells N its offer and path on the announce stream
  when `O(R→N) + cold(R, N)` is below N's route cost for the Instance, or
  equals it through a next hop other than R, since a tie prefers a warm copy.
  It tells again on each change, and withdraws when the demand, its upstream,
  or that test ends. Adverts keep their order with the route layer's
  messages on the same announce stream: a withdrawn route never trails an
  advert sent after it. Nothing it skips between peers could ever be taken: a
  relay at rest costs no more than its own route, and a tie through R is the
  path join already. R knows N's route cost from what N announces it, and
  computes it when N's route runs through R. Skipping the parent and children
  by rule instead left 8 of 400 churned drone meshes unsettled. The mark is
  the route layer's own rule, a route from one marked link never passed to
  another: without it an edge advertised one core's copy to its other core,
  which then pulled through the edge, a path no route offers.
- **The query.** A relay with more than one upstream sends each upstream
  besides the one its path join takes one query when it gains demand: one
  request, one answer. The upstream answers its warm offer and path, or that
  it has none, from what it holds, and keeps nothing: it forwards nothing,
  holds no demand for it, and sends no update. The relay does not wait: it
  joins along its route at once, moves if an answer is strictly cheaper, and
  drops an answer once a pull from that upstream is answered, live or lost:
  an answer kept longer went stale and held a move to a core that had lost
  its upstream. It is not a
  metadata-only subscription: one forwarded by a cold core would hold demand
  of a kind across the core mesh for as long as the viewer stays, the
  standby the epic keeps opt in and off inside the CDN. An upstream cannot
  tell which of its upstreams a dual-homed edge's route uses, since the edge
  announces it nothing, so only the edge can find a warm core beside its
  route.
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
- It takes no advert or answer from a relay named on its own live pull's
  path; its route still counts. That relay's warmth may be this relay's own
  demand coming back: N pulls through X from R, so R has demand besides N
  and offers N its warm cost; N moves to R, X's pull ends, R's offer turns
  cold, N goes back through X, and so on for good. Without the rule, 1 of
  400 churned regions never settled that way under most warm arms; with it,
  none of 2,000 drone meshes or regions and 1 of 2,000 random meshes under
  the pick. That one, case 503, loops through a sibling: 6 pulls from 4,
  which makes 4 warm for 3, so 3 offers 6 less than 4 does; 6 moves to 3, 4
  turns cold for 3, and 6 moves back. 3's offer crosses 6's path at 4, below
  the publisher's relay, but refusing every offer that crosses it left a
  region unsettled under five other arms and did not settle more under the
  pick, so the rule stays as it is and the case is an acceptance test.
  A relay on N's path is upstream of N's parent,
  so N gives up only a move up its own branch, which its route still offers
  when that is the cheaper way. It costs a move to a relay on N's path that
  is warm on its own account, which N cannot tell apart from its own demand:
  in `an_ancestor_warm_on_its_own_is_passed_over` N keeps a tree of 14 where
  13 is cheapest, but under churn the trees it settled on cost about what
  the chain's did, which keeps such moves.
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
Gaining demand lowers offers to others, and the one it lowers to the relay
whose own demand it is, that relay ignores; losing it raises only the offer to
the neighbour that still pulls, which then moves only to something strictly
cheaper. The argument misses what one relay's demand does to a price two
paths share, as case 503 above shows, and the sweep is no proof: all 400
churned meshes of each kind settled under the pick, cycles lasted at most 39
deliveries, each while some answer was in flight, and at 2,000 per kind one
random mesh never settled.

### Simultaneous starts

Two drones reaching the CDN at once each pay their own warm uplink, so
neither offers the other anything cheaper, and both uplinks stay until one
drone's demand ends. The maintainer's rule, decided 2026-11-15: merge them
on the rendezvous hash unless that adds a cycle, an unfed reader, or more
messages than the second uplink costs. Under adverts it does add cycles:
in 400 churned drone meshes, 18 deliveries had a pull the merge made inside
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
  broadcast. A query leaves nothing on the upstream; the edge keeps at most
  one answer per upstream beyond the first per demanded broadcast.
- Check the Goal's efficiency against plain path routing on the same shapes.
  On the simulator: pops 61 against 69 (cheapest tree 59), drones in turn one
  uplink against five, a ring of 48 relays 8 uplinks against 48, tiers 1095
  against 1191 (the cheapest), and an edge beside the warm core 24 against
  29, at 4.3 to 11 messages per reader change against 2 to 5.4. Under churn,
  the trees each case settled on cost 2.7%, 9.4%, and 4.9% over the cheapest
  for the same readers on random meshes, drones, and regions, against path
  joins' 2.7%, 28.9%, and 5.0%.
- Port the growth sweep, and hold its laws, per link kind. Per reader change,
  warm messages are flat in the number of demanded broadcasts for every arm.
  - Between cores, a full peer mesh: flat in cores. A core with demand is
    never cheaper for another core than that core's own route.
  - Down a marked link: none. Up it: one query per upstream beyond the
    first, with its answer, when an edge gains demand, and no advert of a
    copy that came down another. Exactly 1.0 per reader change, one viewer or
    a crowd, flat from 4 to 32 cores and 3 to 128 edges per core, up to
    2,080 relays; a core holds nothing at rest.
  - Between edges or drones that peer: n − 1 per first viewer (63 at 64
    drones), since every one could take the copy; that is what buys the
    cheapest tree there (432 against 1,328 to 1,456 for asking and path
    joins, a crowd of 9 on 16 broadcasts).
  At rest the busiest relay of 80 holds 99 query answers for 4,096
  one-viewer broadcasts, against 4,355 metadata-only subscriptions for
  `ask_1` and 139,264 adverts sent to every neighbour.
- A region's edges do not peer every pair as it grows: they reach each other
  through the region's cores. Peered, the region is a drone mesh and pays its
  n − 1 per first viewer (64 at 64 edges per core); the tree it buys there,
  288 against 544 unpeered for a crowd, comes from pricing an edge-to-edge
  link at a third of an uplink.
- Port the cases as acceptance tests, two of them new: drones starting at
  once, whose uplinks the race keeps, and the same sweeps with each link
  delivering in order at its own pace, rather than one control message per
  delivery step across the mesh, which is what the simulator's unfed counts
  and first frames assume. A first frame matches the path join's everywhere
  the sweeps measured, but two drones a hop apart (81 ms against 69). Two
  edges starting at once each keep their path join (9 against 7), the race
  again. Random churned mesh 503 too, which the simulator's pick never
  settles.
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
the announce stream carrying an Instance's offer and path, and its
withdrawal; the same offer and path on the subscribe answer and its updates;
and a query naming an Instance, answered once with an offer and path or
none. Nothing lite-07 freezes changes.

## Required

- [The switch](/quest/m2/multicast/switch.md) - the route layer it ranks on

## Related

- [Route health](/quest/m3/route-health/README.md) - its metadata-only subscription answers the same way
