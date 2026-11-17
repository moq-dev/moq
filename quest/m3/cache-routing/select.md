# [XL] Relays pull each broadcast from a warm copy and keep it

## Goal

A relay with demand for an Instance advertises it on the announce stream to
each peer or upstream that could take it. When a relay's demand starts, it
pulls from the cheapest copy it has heard of, its route or a warm neighbour.
After that the pull stays: it moves only when its parent loses its upstream,
the pull runs through the relay itself, the session closes, or the relay's
route changes, never because a neighbour's price did. A later step lets an
edge query its other upstreams when it gains demand, once the conditions
below hold. A peer that sends no warm signal is treated as cold and
behaves as it does today. Relays export how many upstream copies they hold
and what each choice was for. Whether a pull instead moves to a cheaper copy
under ranked readers, which also settles, waits on the maintainer (see
Ranked readers below).

## Plan

Rewritten 2026-11-15 (maintainer, /quest-plan on
[cache-aware multicast](/quest/m2/multicast/README.md)): the rule this was
to build, item 14 of the [epic](/quest/m3/cache-routing/README.md)'s earlier
Plan, is replaced.

Decided 2026-11-16 (maintainer, merge pick of
[moq-dev/moq#8903](https://github.com/moq-dev/moq/pull/8903), which
recommended it): **a settled pull stays**, unless the spike the maintainer
asked for with that pick finds a rule that moves a pull for price and still
settles. It found one,
[moq-dev/moq#8986](https://github.com/moq-dev/moq/pull/8986): ranked
readers, below. Which of the two this ports waits on the maintainer; until
then the Plan below stays settled pulls, and the section on ranked readers
says what changes with it. A relay chooses
where to pull when its demand starts, from its route and the warm copies it
has heard of by then. After that it chooses again only on an outside event:
its parent loses its upstream, its pull runs through itself, its session
closes, or its route changes. What it hears comes from **adverts between
peers and toward upstreams, only where the copy could be taken, never down a
link the neighbour marked upstream, and never up one with a copy that came
down one anywhere on its path**, and **the data path and the mark in every offer**. The race
between simultaneous starts stands. **A one-shot query** from an edge to its
other upstreams is a later step, with its price and condition below; with
it, a relay also chooses once more on each query answer from its join.
Rejected: the price-moving pick, which moved a live pull whenever another
source got strictly cheaper and never settles churned random mesh 503.

A rise in the parent's offer is not an event. A rise is what a sibling's
move or a neighbour's lost reader causes, and the point of the rule is that
no relay's move is another relay's reason to move. A route change is one:
the route layer's routes follow links and claims, never demand.

So no price ever moves a pull: a pull moves on an outside event (a reader
starts or stops, a link or relay fails or returns, a publisher moves), on the
route layer settling after one, or on what those set off. It is greedy online
Steiner without rearrangement, apart from the moves a route change buys.
A TLA+ model shows it settles at small bounds; see below.

**What the guarantee costs.** Imase and Waxman (Dynamic Steiner Tree
Problem, SIAM J. Discrete Math. 4(3), 1991) bound greedy joins without
rearrangement at O(log k) times the cheapest tree for k terminals, and show no
online rule without rearrangement does better than ½ log2 k. The bound
assumes terminals only arrive, fixed link costs, and each newcomer joining the
nearest node of the whole tree by a shortest path. Here a relay joins the
cheapest copy it has heard of, priced by the copy's whole path rather than
the one link it adds, and readers also leave; for arrivals and departures
together their constant-factor algorithm rearranges O(n^(3/2)) edges over n
requests, which this never does for price. So the bound is a guide, and the
sweeps measure the drift:

| | settled pulls, first cut | with the query, later | ranked readers | the price-moving pick | path joins |
|---|---|---|---|---|---|
| settled trees over the cheapest, 400 churned random / drone / region meshes | +3.4% / +16.2% / +5.4% | +3.4% / +16.2% / +5.0% | +1.2% / +4.9% / +4.4% | +2.7% / +9.4% / +4.9% | +2.7% / +28.9% / +5.0% |
| unsettled of 2,000 per kind | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 1 / 0 / 0 (random case 503) | 0 / 0 / 0 |
| longest cycle under churn, deliveries | 20 / 18 / 14 | 20 / 18 / 19 | 19 / 23 / 18 | 18 / 39 / 24 | 16 / 14 / 17 |
| unfed reader-hops | 19,008 / 11,691 / 17,470 | 19,008 / 11,691 / 23,089 | 21,917 / 14,584 / 20,661 | 18,000 / 12,381 / 22,472 | 15,700 / 7,725 / 15,735 |
| warm messages, 400 churned regions | 4,655 | 21,020 | 13,133 | 21,151 | 0 |
| pops, summed over its five phases (cheapest 59) | 71 | 71 | 59 | 61 | 69 |
| drones in turn | 75, one uplink | 75, one uplink | 75, one uplink | 75, one uplink | 155, five |
| drones / ring / edges at once (cheapest 19 / 112 / 7) | 51 / 392 / 9 | as the first cut | 19 / 112 / 8 | 51 / 392 / 9 | 51 / 392 / 9 |
| drones lose the uplink (cheapest 94) | 118, four uplinks, a 17-step cycle | 118, four uplinks, a 17-step cycle | 94, one uplink | 118, four uplinks | 198 |
| drones one / two hops apart | 41 / 33, at 69 / 62 ms | as the first cut | 17 / 17, at 81 / 69 ms | 33 / 25, at 81 / 69 ms | 41 / 41, at 69 / 62 ms |
| edges in turn, one region with unequal uplinks, rings, tiers | 13, 29, 2904, 1095 | 12, 24, 2904, 1095 | 13, 29, 2904, 1095 | 12, 24, 2904, 1095 | 14, 29, 9624, 1191 |
| warm messages per reader change, unpeered CDN, one viewer / crowd | 0 / 0 | 1.0 / 1.0 | 0 / 0.67 | 1.0 / 1.0 | 0 / 0 |
| warm messages per reader change, 64 peered drones, a crowd | 63 | 63 | 161 | 70 | 0 |

Random and drone meshes have no relay with two upstreams, so the query
changes nothing there. Ranked readers were measured on 2026-11-17 beside a
rerun of the other columns, which matched the table; at 2,000 per kind they
settle +1.4% / +6.3% / +4.6% over the cheapest, against the first cut's
+3.2% / +16.0% / +5.3%.

The table is the rerun of 2026-11-16, after the route order gained the mark
([moq-dev/moq#8904](https://github.com/moq-dev/moq/pull/8904)) and a
request's equal next hops follow it. Every
settled tree and unsettled count is as first measured; the churn rows moved
by under 1%, apart from path joins' longest cycle on random meshes, 9 before.

Its drift is a relay that stays on a detour once the warm copy that drew it
loses its other readers: on pops, the middle pop keeps pulling through the
near pop after the near pop's reader leaves (71 against path joins' 69).
Choosing again when a relay's own reader starts (`+rejoin`) changed nothing
measurable; choosing again on a route change (`+reroute`, part of the
recommendation) took random meshes from +4.4% to +3.4% and regions from
+6.0% to +5.0%. Drone meshes drift most, since their routes rarely change.
No timer is added.

**The price-moving pick, rejected,** moved a live pull whenever another source got
strictly cheaper, fenced by a rule that a relay takes no warm signal from a
relay on its own path. It never settles churned random mesh 503, a loop
through a sibling: 6 pulls from 4, which makes 4 warm for 3, so 3 offers 6
less than 4 does; 6 moves to 3, 4 turns cold for 3, and 6 moves back. Each
fence covered one shape of a relay's own demand coming back as a price, as
the old route rule's fixes did; refusing every offer whose path crosses the
taker's below the publisher's relay unsettled other arms instead. Under the
recommendation no price moves a pull, so the reflection can make a choice
wrong but cannot make one. Whether the choices a lost or failed pull sets
off can repeat is not yet shown, as below.

Also rejected:

- Advertising to every neighbour that could take the copy, down marked links
  too: it grows with a region's edges, since a warm core tells every edge it
  could feed. 1.7 to 118 warm messages per one-viewer reader change as edges
  per core grow from 3 to 128, against none in the first cut and a flat 1.0
  with the query.
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
  an offer leaves them out: another fence for the pick's loops. It adds a
  list and an update message on each change (6,904 to 8,485 per 400 churned
  meshes) and still left 1 of 2,000 churned random meshes unsettled.
- Asking the k cheapest neighbours on each SUBSCRIBE. It stays bounded per
  subscribe only because it stops finding the copy: on a full drone mesh
  past 5 drones, `ask_1` shared no uplink at 64 drones and `ask_2` few. It
  doubled the time to a first frame and left about twice the readers unfed
  under churn. On CDN tiers `ask_1` sent 5.7 to 7 warm messages per
  one-viewer reader change against 1.0.
- Advertising only once two others want the copy, which lost both cases;
  never advertising over a link the sender marked upstream, which saves
  nothing in a core mesh.
- Joins along the path only, the cost baseline, which delivers neither case.

That PR's simulator, `rs/moq-net/src/route/tests/warm.rs`, runs the first
cut as `advertise_not_down+hops+stay+reroute` and the later step as
`advertise_not_down_query+hops+stay+reroute`, and its `warmth.rs`
holds the shapes, the churn sweep, and the growth sweep this ports onto
production code. Ranked readers run as `advertise_not_down+hops+order_route`.

### Ranked readers, waiting on the maintainer

[moq-dev/moq#8986](https://github.com/moq-dev/moq/pull/8986) spiked a rule
under which a pull moves to a cheaper warm copy that turns up later, and
`rs/moq-net/verify/warm/` (the `verify-warm` skill) model-checks that it
settles.

- **The rank.** A relay with a reader of its own for an Instance ranks it
  by its route cost to the Instance, then its Node ID. A pull carries the
  lowest rank among the readers it feeds, its relay's own and each
  downstream pull's, and an update when that changes. An advert and a
  subscribe answer carry the lowest rank their sender feeds besides their
  receiver.
- **The choice.** A relay takes a warm copy only from a neighbour feeding a
  reader ranked strictly below every reader the relay feeds itself. That
  copy is fed whatever the relay does, so it costs the link's cold price
  alone. The relay heads for the cheapest such copy or its route, make
  before break, whenever that set or its own rank changes; nothing else
  moves it, and no price above the first hop enters the choice.
- **Why it settles.** A relay's choice reads only copies feeding lower
  ranks, and what those feed never depends on a higher rank's choice. So
  once the outside events stop, the lowest rank's pulls settle, then the
  next, by induction on the rank: no relay's demand comes back to it as a
  price, the reflection every fence of the pick chased. TLC checks it from
  every state the bounded outside events reach, delivering every word late
  and in any order, on three drones with three reader changes, two fixed
  readers while an uplink closes and reopens, and churned random mesh 503's
  six relays.
  With the rank check removed it fails on the three drones: two readers'
  relays each take the other's copy, find their own path in it, fall back,
  and repeat. The pick fails on case 503, as the sweep found.
- **What stays from the first cut.** Adverts, the mark, the path, the
  answer's offer, and choosing again on a route change. Adverts still go
  only where the copy could be taken, priced from the publisher (offer plus
  the link's cold price, against the neighbour's route); pricing that test
  at the link alone made a full core mesh's warm messages grow with its
  cores (0.9 to 28.6 per reader change from 4 to 32 cores), where priced
  from the publisher they stay flat: none for one viewer, 0.67 for a crowd. The path still breaks
  a cycle formed on stale adverts: without it no mesh went unsettled, but
  the longest cycle on drones grew from 29 to 99 deliveries.
- **What it buys.** The pop that turns warm later draws the pulls behind it
  (pops 59, the cheapest, against 71). Drones, rings, and edges that start
  at once merge onto one uplink (19 against 51 on drones, 112 against 392 on
  the ring): the higher rank moves, the merge the maintainer's rule of
  2026-11-15 asked for, here without the cycles it added under the pick. A drone that loses the uplink
  hands it to one other drone, not four, and without the first cut's
  17-step cycle. Churned trees settle at a third of the first cut's drift on
  drones and random meshes.
- **What it costs.** On the wire, in the then-current wip version: a rank
  on each SUBSCRIBE and an update message when it changes; a rank on each
  advert and subscribe answer and its updates. A rank is a route cost and a
  Node ID. Relay state: one rank per served subscription, one per heard
  advert or answer, and the rank last told on each pull, all bounded as the
  adverts are. Messages: 13,133 warm messages per 400 churned regions
  against 4,655, rank updates 8,494 of them; 0.67 per crowd reader change on
  an unpeered CDN against none; about 2.6 times the first cut's per reader
  change between peers (161 against 63 for a crowd on 64 drones), since a
  rank change reaches every peer that could take the copy. Readers: moves
  stall readers in the simulator, which delivers one control message per
  step across the mesh, so unfed reader-hops rise by 15% to 25%, and a
  drone one hop from another waits 81 ms for a first frame against 69.
- **A peer that sends no rank** is ranked below every ranked reader: a
  relay it pulls from never takes a warm copy for its sake and pulls along
  its route, as today. Its adverts are never taken, since nothing says the
  copy is fed regardless.
- **Tried and dropped on the way.** Pricing a copy at the parent's summed
  offer plus the link, as the first cut does, and keeping the current copy
  while no dearer: it reads demand above the first hop, so relays kept
  dearer copies (drones +18.4% against +4.9%). Ranking by Node ID alone: it
  settles too (`Hash.cfg`, and 0 of 6,000 churned meshes unsettled under an
  earlier advert audience), but the near pop never hears a middle pop
  ranked above it, so pops keeps the path join's tree.

Recommended: port ranked readers rather than settled pulls. Both settle in
every sweep and pass the model; ranked readers move toward the cheapest
tree where warmth matters, between peers and at simultaneous starts, and
match the first cut's trees on an unpeered CDN, for one more field on three
messages and one update message. Settled pulls stay the choice if that
wire or the extra moves are not worth a third of the drift.

### Decision: the query, a later step

Recommended: ship adverts, the mark, the path, and settled pulls first, and
add the query as a later step; an automated review on #8903 proposed the
same. The query is the one part that only an edge with more than one
upstream uses.

- **What it buys:** only where an edge's cheaper uplink is to the core that
  is cold. One region with unequal uplinks costs 24 with it and 29 without,
  and two edges in turn 12 against 13; churned regions settle +5.0% over the
  cheapest with it against +5.4% without (+5.1% against +5.3% at 2,000 per
  kind). With equal uplinks it buys nothing: every edge of a region ranks the
  two cores by the same rendezvous hash per broadcast, so every edge's path
  join lands on the same core.
- **What it costs:** a request and an answer on the wire, and exactly 1.0
  warm message per reader change on an unpeered CDN against none. Under
  churn, per 400 regions, 21,020 warm messages against 4,655, 59,921
  messages against 45,280, and 23,089 unfed reader-hops against 17,470, much
  of that from the simulator delivering one control message per step across
  the mesh. The edge holds one answer per extra upstream per demanded
  Instance; the upstream holds nothing and carries no traffic. It also adds a
  choice after the join that the settling model must cover.
- **Until then** an operator gets the same tree by pricing each edge's
  uplinks equally.
- **Its condition:** add it once the settling model holds with the query in
  it, the edge's bound of one answer per extra upstream per demanded
  Instance, and none on the upstream, is enforced and tested, and a
  deployment has unequal uplinks where the core-mesh copy it saves matters.
  Shipping it in the first cut instead is measured above, and settles too.

### What each part costs

| part | on the wire | relay state | bounded by | without it |
|---|---|---|---|---|
| advert | offer, path, and mark on the announce stream, in order with the route messages, and its withdrawal | one heard per neighbour per Instance the relay has a route for | neighbours; the path by the hop limit | path joins: drones in turn 5 uplinks against 1, a ring 48 against 8, tiers 1191 against 1095 |
| the route layer's mark on adverts | one bit on the advert, the answer, and the query's answer, as a route's offer carries | the bit with each heard advert and live pull | one per offer | an edge carries a copy between its cores, which no route would: 51 readers in 400 churned regions fed that way under the pick, and 1.05 to 1.55 warm messages per one-viewer reader change on an unpeered CDN against 1.0 |
| offer on the subscribe answer | a cost, path, and mark on the answer, and an update on each change; none when the parent lost its upstream | one per pull | one per subscription | nothing prices a query answer or a new route against the pull, and a lost parent cannot say so |
| path in the offer | a list on the advert, the answer, and the query's answer | as the offer | the hop limit | 2 of 6,000 churned meshes unsettled (random 433, drones 1783), longest cycle 109 deliveries against 27: a pull cycle formed on stale adverts cannot be seen |
| query, the later step | a request naming an Instance; one answer, offer, path, and mark or none | the edge keeps one answer per extra upstream per demanded Instance until judged; the upstream none | upstreams beyond the first | an edge whose cheaper uplink is cold: 29 against 24 |
| a settled pull stays | none | the route it chose by | one choice per outside event | the pick: random churned mesh 503 never settles |
| choosing again on a route change | none | as above | route changes, which the route layer bounds | settled trees +4.4% against +3.4% on random meshes, +6.0% against +5.0% on regions |

What the pick needed and this deletes: the rule against a warm signal from a
relay on the taker's own path, since a relay that never moves for price
cannot be drawn back up its own branch; the rule that a move under way stays
while no dearer; and make-before-break bookkeeping for price moves, kept only
for a query answer judged after the pull went live. A lost pull carries
nothing, so a new one replaces it outright.

### What a relay sends

- **The offer.** What a relay R with a live upstream through parent P offers
  neighbour N: `O(R→N) = O(P→R) + warm(P, R)` while R has demand besides N,
  otherwise `O(P→R) + cold(P, R)`, the chain priced unshared. The publisher's
  relay offers 0. An offer is never below its sender's own cost.
- **The path and the mark.** Every offer names the relays the data passes
  from the publisher's relay to the sender, and carries the mark: set once
  the copy came down a link its receiver marked upstream anywhere on that
  path, and kept on every hop after, as a route's down-only flag is.
- **Adverts.** While R has demand for an Instance besides N, N has not
  marked R upstream, and R has not both marked N upstream and a copy whose
  mark is set, it tells N its offer on the announce stream when
  `O(R→N) + cold(R, N)` is below N's route cost for the Instance, or equals it
  through a next hop other than R, since a tie prefers a warm copy. It tells
  again on each change, and withdraws when the demand, its upstream, or that
  test ends. Adverts keep their order with the route layer's messages on the
  same announce stream. Nothing it skips between peers could ever be taken: a
  relay at rest costs no more than its own route, and a tie through R is the
  path join already. R knows N's route cost from what N announces it, and
  computes it when N's route runs through R. The mark is the route layer's
  own rule: without it an edge advertised one core's copy to its other core,
  which then pulled through the edge, a path no route offers.
- **The query, the later step.** A relay with more than one upstream sends each upstream
  besides the one its path join takes one query when it gains demand: one
  request, one answer. The upstream answers its warm offer, or that it has
  none, from what it holds, and keeps nothing: it forwards nothing, holds no
  demand for it, and sends no update. It is not a metadata-only
  subscription: one forwarded by a cold core would hold demand of a kind
  across the core mesh for as long as the viewer stays, the standby the epic
  keeps opt in and off inside the CDN. An upstream cannot tell which of its
  upstreams a dual-homed edge's route uses, so only the edge can find a warm
  core beside its route.
- **Answers.** The subscribe answer carries the parent's offer, path, and
  mark, an update on each change, and none once the parent lost its own
  upstream, which it says at once to a new subscriber too. A subscriber waits
  for that answer before it counts the pull as live.

### What a relay chooses

- **When its demand starts**, it pulls from the cheapest of its route and
  the warm copies it has heard of. A copy costs its offer plus the link's
  cold price; the route costs the route layer's summed cold cost. Equal
  candidates fall to a warm copy before the route, then the rendezvous hash,
  as `route_order` in `rs/moq-net/src/model/origin.rs` already ranks equal
  routes. It joins at once, without waiting for its queries.
- **On each query answer from that join**, once the query lands, judged once, it moves to the
  answering upstream if that is strictly cheaper than its own cost
  `O(P→R) + warm(P, R)`, keeping the old pull until the new one is live.
- **When its route changes**, it chooses once more, as at a join, from what
  it has heard by then, moving only to something strictly cheaper. A route
  change can reach a relay before the advert that came with it; then the one
  choice finds only the route (`an_ancestor_warm_on_its_own_is_passed_over`
  keeps 14 where 13 is cheapest).
- **When its parent answers none, or its pull's path names it**, it replaces
  the pull outright with a fresh choice.
- **Otherwise it stays**, whatever its parent or anyone else offers later.
- It never pulls from a neighbour that pulls through it, nor from one whose
  path names it. A pending pull whose source left its options fails closed:
  two relays that each turned to the other on stale adverts otherwise wait on
  each other for good. The longest such cycle in 2,000 churned meshes per
  kind lasted 27 deliveries on random meshes, 44 on drones, and 30 on
  regions (19 with the query); the lost-uplink case shows one for 17 steps.

Why it should settle, and what is not yet shown: after the last outside
event, a relay chooses only as the route layer settles (finitely many route
changes, which its own TLA+ model and sweeps cover), with the query on each
answer from a join (one per extra upstream), and when a pending pull fails closed
or a parent says it lost its upstream. No choice follows a price. But a
fresh choice after a lost parent or a failed pending pull can pick a warm
copy on a stale advert, give that relay demand and so a join of its own, or
end another relay's pull, so "once per pull" does not bound the number of
pulls after the last outside event (an automated review on #8903). The sweep
agrees with settling: 6,000 churned meshes, 2,000 per kind, all settled.
The `verify-warm` model checks it at small bounds: settled pulls go quiet
and feed every routed reader on churned random mesh 503 and on three drones
with three reader changes (`Settled.cfg`, `SettledDrones.cfg`). It found no
warm retries that repeat, so the rule takes no fallback.

### Simultaneous starts

Two drones reaching the CDN at once each pay their own uplink, and both stay
until one drone's demand ends; under the recommendation nothing merges them,
since a merge is a move for price. The maintainer's rule, decided 2026-11-15,
was to merge them on the rendezvous hash unless that adds a cycle, an unfed
reader, or more messages than the second uplink costs; under the pick's
adverts it added cycles (18 deliveries through merged pulls in 400 churned
drone meshes) and about 1% unfed reader-hops. So the race stands under settled
pulls; ranked readers merge them (above). A drone
that loses the one uplink leaves every drone it fed choosing again at once:
4 uplinks on the five-drone shape.

### What holds

- A warm link costs its warm price and a cold one its cold price, both at
  least 1, summed from the source along the path the data takes.
- Extend today's selection rather than adding a second selector. A front
  already moves to a better route of its own Instance and resplices
  (`rs/moq-net/src/model/front.rs`); here it does so only on the events
  above. A front moves only to a copy of its own Instance.
- The rule runs in production code that the fixtures and sweeps drive. The
  stripped rule lived only in a test simulator that checked it against
  itself, so a rule that never offered a copy passed. Port the warmth shapes
  and the churn sweep onto production relays: pops, drones in turn and at
  once, the lost uplink, the ring, the tiers, and random churned mesh 503.
- Port the rule the `verify-warm` model checks (`Rule = "stay"` for settled
  pulls, `"order"` for ranked readers), and move its Coverage table from the
  simulator onto the port, keeping the model in step with it. The query is
  enabled only once the model holds with it.
- Bound the warm state a peer can make a relay hold: one advert per
  neighbour per Instance the relay has a route for, a path no longer than the
  route layer's hop limit, and adverts for anything else dropped. Adverts are
  per broadcast like the claims the announce stream already carries over
  every link, so a relay hears at most one per neighbour per demanded
  broadcast. A query leaves nothing on the upstream; the edge keeps at most
  one answer per upstream beyond the first per demanded broadcast.
- Check the Goal's efficiency against plain path routing on the same shapes,
  as the table above does: on the simulator, 3.4 to 11 messages per reader
  change against 2 to 5.4, and under churn the trees settled on cost 3.4%,
  16.2%, and 5.4% over the cheapest (5.0% on regions with the query) for the same readers on random meshes,
  drones, and regions, against path joins' 2.7%, 28.9%, and 5.0%.
- Port the growth sweep, and hold its laws, per link kind. Per reader change,
  warm messages are flat in the number of demanded broadcasts.
  - Between cores, a full peer mesh: flat in cores. A core with demand is
    never cheaper for another core than that core's own route.
  - Down a marked link: none. Up it: no advert of a copy that came down
    another, so none in the first cut. With the query, one per upstream
    beyond the first, with its answer, when an edge gains demand: exactly 1.0
    per reader change, one viewer or a crowd, flat from 4 to 32 cores and 3
    to 128 edges per core, up to 2,080 relays; a core holds nothing at rest.
  - Between edges or drones that peer: n − 1 per first viewer (63 at 64
    drones), since every one could take the copy; that is what buys the
    cheapest tree there (432 against 1,328 to 1,456 for asking and path
    joins, a crowd of 9 on 16 broadcasts).
  With the query, at rest the busiest relay of 80 holds 99 query answers
  for 4,096 one-viewer broadcasts, against 4,355 metadata-only subscriptions for
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
  and first frames assume. A first frame matched the path join's on every
  shape measured. Two edges starting at once each keep their path join (9
  against 7), the race again.
- Export upstream copies held, choices by cause (join, route change, parent
  lost its upstream, and query answer once it lands), and adverts sent and held, through the
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
the announce stream carrying an Instance's offer, path, and mark, and its
withdrawal, in order with the route messages there; the same offer, path, and
mark on the subscribe answer and its updates; and, with the later step, a
query naming an Instance, answered once with an offer, path, and mark or
none. With ranked readers, also a rank on the SUBSCRIBE, the advert, and
the answer, and an update message for a subscription's rank. Nothing
lite-07 freezes changes.

## Required

- [The switch](/quest/m2/multicast/switch.md) - the route layer it ranks on

## Waiting

- Maintainer: port ranked readers or settled pulls (see Ranked readers,
  waiting on the maintainer). Recommended: ranked readers.

## Related

- [Route health](/quest/m3/route-health/README.md) - its metadata-only subscription answers the same way
