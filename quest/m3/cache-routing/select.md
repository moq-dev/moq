# [XL] Relays pull each broadcast from a warm copy, ranked by reader

## Goal

A relay with demand for an Instance advertises it on the announce stream to
each peer or upstream that could take it. A relay with a reader of its own
ranks it by its route cost to the Instance, then its Node ID, and every
pull, advert, and subscribe answer carries the lowest rank its sender feeds.
A relay pulls from the cheapest of its route and the warm copies that feed a
reader ranked below every reader it feeds, and moves, make before break,
whenever that set or its own rank changes. Once readers, links, and
publishers stop changing, the pulls settle. A later step lets an edge query
its other upstreams when it gains demand, once the conditions below hold. A
relay that sends no rank or warm signal is treated as cold and behaves as it
does today, and a relay it pulls through takes its route. Relays export how many
upstream copies they hold and what each choice was for. The wire lands on
lite-07-wip if the port has landed and passed the mesh replay when the cut
is made, and otherwise in the first wip version after the cut.

## Plan

Rewritten 2026-11-15 (maintainer, /quest-plan on
[cache-aware multicast](/quest/m2/multicast/README.md)): the rule this was
to build, item 14 of the [epic](/quest/m3/cache-routing/README.md)'s earlier
Plan, is replaced.

Decided 2026-11-16 (maintainer, merge pick of
[moq-dev/moq#8903](https://github.com/moq-dev/moq/pull/8903)): a settled
pull stays, unless a spike finds a rule that moves a pull for price and
still settles. What a relay hears comes from **adverts between peers and
toward upstreams, only where the copy could be taken, never down a link the
neighbour marked upstream, and never up one with a copy that came down one
anywhere on its path**, and **the data path and the mark in every offer**.
**A one-shot query** from an edge to its other upstreams is a later step,
with its price and condition below. Rejected: the price-moving pick, which
moved a live pull whenever another source got strictly cheaper and never
settles churned random mesh 503.

Decided 2026-11-17 (maintainer, on
[moq-dev/moq#8986](https://github.com/moq-dev/moq/pull/8986#issuecomment-6948028151),
the spike, which recommended it): ✅ **ranked readers** (rejected: settled
pulls, which give up about two thirds of the drift for a small wire saving).
Adverts, the mark, the path, and the query as a later step stand from
2026-11-16.

Decided 2026-11-17 (maintainer, same comment): ✅ **warmth ships in the next
wip version, after the lite-07 cut** (rejected: holding lite-07 for the
port; freezing the rank and advert fields in lite-07 before the port has
exercised them). The 2026-11-14 decision stands: lite-07 freezes without a
warm wire. The route layer it ranks on shipped in
[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987), and this port
is the first item after the cut.

Decided 2026-11-19 (maintainer, in the coordinating session): ✅ **the port
starts now, and the cut does not wait for it** (rejected: blocking the cut
on warmth; starting only after the cut, as decided 2026-11-17). The cut now
waits on [the mesh replay](/quest/m2/multicast/mesh-replay.md) and the
flaws the audit found, so the port has that long. If it has landed and been
through the mesh replay when the cut is made, its wire is in lite-07; if
not, lite-07 freezes without it and the wire moves to the next wip version.
[The cut](/quest/m2/lite07-finalize.md) checks it as one of its steps. The
reason for the change: a deployment expects warm pulls to cut what its
backbone carries by a large part, and a release is the only way its
production mesh gets them.

Decided 2026-11-19 (the coordinating session, to which the maintainer
delegated the call, on
[moq-dev/moq#9157](https://github.com/moq-dev/moq/pull/9157#issuecomment-7005346052)):
✅ **the measurement lands as the port's first step, and the port is not
built until the maintainer answers the item under Waiting** (rejected:
building the port now regardless; deciding there to drop or defer it).

Measured 2026-11-18, before building. `warmth.rs` reads a
mesh from a file (relays, links with costs, marks, and a label, and where
publishers and readers sit) and `benchmark_warmth_mesh` reports the
link-groups ranked readers and path joins each put on it, in all, per label,
and per link. A deployment's graph is private: pass its file by path as
`MOQ_WARMTH_MESH` and commit none. `warmth.mesh` beside it is the example.

A tree feeding the same readers spans as many links under either rule, give
or take a relay carrying for others, so the saving is in which links carry
it. Generated meshes, five seeds each summed, every one settled right under
both rules, path joins / ranked readers:

| mesh | backbone link-groups | backbone cold cost | every link-group | control messages |
|---|---|---|---|---|
| CDN, full core mesh, equal uplinks | 95 / 95 | 1048 / 760 (-27%) | 339 / 339 | 718 / 2902 |
| CDN, full core mesh, unequal uplinks | 125 / 90 (-28%) | 1408 / 736 (-48%) | 368 / 368 | 776 / 3193 |
| CDN, ring of regions, equal uplinks | 97 / 95 (-2%) | 776 / 760 (-2%) | 341 / 339 | 722 / 2234 |
| CDN, ring of regions, unequal uplinks | 134 / 94 (-30%) | 1072 / 752 (-30%) | 377 / 372 | 794 / 2735 |
| CDN, one hub region, equal uplinks | 97 / 97 | 1088 / 1088 | 341 / 341 | 722 / 1914 |
| CDN, one hub region, unequal uplinks | 128 / 93 (-27%) | 1432 / 1052 (-27%) | 371 / 371 | 782 / 2482 |
| 8 relays with an uplink each, peered every pair | 71 / 20 (-72%) | 710 / 200 (-72%) | 71 / 71 | 182 / 1236 |
| 8 relays with an uplink each, peered in a ring | 71 / 42 (-41%) | 710 / 420 (-41%) | 71 / 71 | 182 / 508 |

A CDN here is six regions on a circle, two cores each, and four edges a
region marking both cores upstream; the backbone is the links between
regions, priced 4 plus 4 per step around the circle, warm a third. Four
broadcasts are published at random edges and each read by a random half of
the others. Equal uplinks cost 3, unequal ones 3 or 4 at random. In the last
two rows the backbone is the uplinks to the publisher's relay (10 cold, 4
warm, peer links 2), and half the relays read each of four broadcasts.

What it shows:

- With equal uplinks a region's edges already land on one core by the
  rendezvous hash, the path joins already share the shortest-path tree, and
  warmth saves no backbone link-group: 0% to 2%.
- On a full core mesh it carries as many link-groups over shorter links: a
  far region pulls from a nearer warm one instead of straight from the
  publisher's, 27% off the cost where a link is priced by its length and
  nothing where every backbone link costs the same. Each such pull adds a
  relay hop.
- With unequal uplinks a region's edges split over its cores and each core
  pulls its own copy: 27% to 30% fewer backbone link-groups. Pricing each
  edge's uplinks equally gets the same tree with no wire.
- Relays with an uplink each and links between them, the drone meshes, save
  41% to 72%, the whole of what item 9 of the
  [epic](/quest/m2/multicast/README.md) asked for.
- It costs 2.6 to 6.8 times the control messages.

On a deployment's graph, measured by path and not committed: under its
hop-count link costs, ranked readers cut backbone link-groups 4% against
path joins. With latency link costs and a warm price at a third of the cold,
they cut 20% (39% of latency-weighted backbone cost, 46% of intercontinental
link-groups), at +7 to 21% mean reader latency and up to 8.7x the control
messages. With latency costs and no warm discount, 2%. Latency link costs
alone, with path joins, remove a 5 to 12% mean reader-latency stretch over
the shortest path, with no wire change; the relay's cluster config already
prices each peer link (`cost`).

**Why ranked readers settle.** A relay's choice reads only copies feeding
lower ranks, and what those feed never depends on a higher rank's choice.
Once the outside events stop and the route layer settles (its own TLA+
model and sweeps cover that), every rank is fixed; then the lowest rank's
pulls settle, then the next, by induction on the rank. No relay's demand
comes back to it as a price, the reflection every fence of the pick chased.
`rs/moq-net/verify/warm/` (the `verify-warm` skill) checks it with TLA+ from
every state the bounded outside events reach, delivering each neighbour's
words late: what one relay tells a neighbour (its pull's rank, its answer,
its advert) arrives together as that neighbour's newest word, any number of
steps stale, and neighbours fall behind independently. It never delivers one
neighbour's new rank beside its old advert or answer, which the wire can,
since they ride separate streams; the port covers that, below. The model
checks three drones with three reader changes, two fixed readers
while an uplink closes and reopens, and churned random mesh 503's six
relays. With the rank check removed it fails on the three drones: two
readers' relays each take the other's copy, find their own path in it, drop
it, and repeat. The pick fails on case 503, as the sweep found.

**What the guarantee costs.** Imase and Waxman (Dynamic Steiner Tree
Problem, SIAM J. Discrete Math. 4(3), 1991) bound greedy joins without
rearrangement at O(log k) times the cheapest tree for k terminals, and show no
online rule without rearrangement does better than ½ log2 k. The bound
assumes terminals only arrive, fixed link costs, and each newcomer joining the
nearest node of the whole tree by a shortest path. Ranked readers rearrange:
once settled, each relay's pull depends only on lower-ranked readers, as if
readers had arrived in rank order whatever order they really came in, so a
departure re-forms the tree rather than leaving the detour an arrival-only
rule keeps. The bound is still a guide, since a relay joins only copies it
has heard of, priced by the link alone; the sweeps measure the drift:

| | ranked readers, the port | settled pulls, rejected | with the query, measured under settled pulls | the price-moving pick, rejected | path joins |
|---|---|---|---|---|---|
| settled trees over the cheapest, 400 churned random / drone / region meshes | +1.2% / +4.9% / +4.4% | +3.4% / +16.2% / +5.4% | +3.4% / +16.2% / +5.0% | +2.7% / +9.4% / +4.9% | +2.7% / +28.9% / +5.0% |
| unsettled of 2,000 per kind | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 1 / 0 / 0 (random case 503) | 0 / 0 / 0 |
| longest cycle under churn, deliveries | 19 / 23 / 18 | 20 / 18 / 14 | 20 / 18 / 19 | 18 / 39 / 24 | 16 / 14 / 17 |
| unfed reader-hops, in the simulator's clock (below) | 21,917 / 14,584 / 20,661 | 19,008 / 11,691 / 17,470 | 19,008 / 11,691 / 23,089 | 18,000 / 12,381 / 22,472 | 15,700 / 7,725 / 15,735 |
| warm messages, 400 churned regions | 13,133 | 4,655 | 21,020 | 21,151 | 0 |
| pops, summed over its five phases (cheapest 59) | 59 | 71 | 71 | 61 | 69 |
| drones in turn | 75, one uplink | 75, one uplink | 75, one uplink | 75, one uplink | 155, five |
| drones / ring / edges at once (cheapest 19 / 112 / 7) | 19 / 112 / 8 | 51 / 392 / 9 | as settled pulls | 51 / 392 / 9 | 51 / 392 / 9 |
| drones lose the uplink (cheapest 94) | 94, one uplink | 118, four uplinks, a 17-step cycle | 118, four uplinks, a 17-step cycle | 118, four uplinks | 198 |
| drones one / two hops apart | 17 / 17, at 81 / 69 ms | 41 / 33, at 69 / 62 ms | as settled pulls | 33 / 25, at 81 / 69 ms | 41 / 41, at 69 / 62 ms |
| edges in turn, one region with unequal uplinks, rings, tiers | 13, 29, 2904, 1095 | 13, 29, 2904, 1095 | 12, 24, 2904, 1095 | 12, 24, 2904, 1095 | 14, 29, 9624, 1191 |
| warm messages per reader change, unpeered CDN, one viewer / crowd | 0 / 0.67 | 0 / 0 | 1.0 / 1.0 | 1.0 / 1.0 | 0 / 0 |
| warm messages per reader change, 64 peered drones, a crowd | 161 | 63 | 63 | 70 | 0 |

At 2,000 churned meshes per kind, ranked readers settle +1.4% / +6.3% /
+4.6% over the cheapest, against settled pulls' +3.2% / +16.0% / +5.3%.
Random and drone meshes have no relay with two upstreams, so the query
changes nothing there. The settled-pulls, query, pick, and path-join columns
are the rerun of 2026-11-16, after the route order gained the mark
([moq-dev/moq#8904](https://github.com/moq-dev/moq/pull/8904)); ranked
readers were measured on 2026-11-17 beside a rerun that matched them.

The simulator, `rs/moq-net/src/route/tests/warm.rs`, runs ranked readers as
`advertise_not_down+hops+order_route`, settled pulls as
`advertise_not_down+hops+stay+reroute`, and the query as
`advertise_not_down_query+hops+stay+reroute`; its `warmth.rs` holds the
shapes, the churn sweep, and the growth sweep this ports onto production
code.

**Settled pulls, rejected.** A relay chose where to pull when its demand
started and chose again only on an outside event: its parent lost its
upstream, its pull ran through itself, its session closed, or its route
changed, never because a price did. It settles too: 0 of 6,000 churned
meshes unsettled, and `Settled.cfg` and `SettledDrones.cfg` pass the same
model. Its drift is a relay that stays on a detour once the warm copy that
drew it loses its other readers (pops 71 against the cheapest 59), and two
relays that start at once each keep their own uplink (drones 51 against 19).
It needs no rank on the wire and sends fewer messages between peers, which
the maintainer judged not worth about two thirds of the drift.

**The price-moving pick, rejected,** moved a live pull whenever another
source got strictly cheaper, fenced by a rule that a relay takes no warm
signal from a relay on its own path. It never settles churned random mesh
503, a loop through a sibling: 6 pulls from 4, which makes 4 warm for 3, so 3
offers 6 less than 4 does; 6 moves to 3, 4 turns cold for 3, and 6 moves
back. Each fence covered one shape of a relay's own demand coming back as a
price, as the old route rule's fixes did. The rank removes the cause rather
than a shape of it.

Also rejected:

- For ranked readers, pricing a copy at the parent's summed offer plus the
  link, and keeping the current copy while no dearer: it reads demand above
  the first hop, so relays kept dearer copies (drones +18.4% against +4.9%).
- Ranking by Node ID alone, or a hash of it: it settles too, but the near
  pop never hears a middle pop ranked above it, so pops keeps the path
  join's tree.
- Pricing the advert audience at the link alone, as the choice prices a
  copy: a warm core told every core it meshes with, 0.9 to 28.6 warm
  messages per reader change from 4 to 32 cores.
- Advertising to every neighbour that could take the copy, down marked links
  too: it grows with a region's edges, since a warm core tells every edge it
  could feed. 1.7 to 118 warm messages per one-viewer reader change as edges
  per core grow from 3 to 128, against none with adverts as decided and a
  flat 1.0 with the query.
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

### The query, a later step

Decided 2026-11-16 with settled pulls; it stands under ranked readers. The
query is the one part that only an edge with more than one upstream uses.

- **What it buys:** only where an edge's cheaper uplink is to the core that
  is cold. Under settled pulls one region with unequal uplinks cost 24 with
  it and 29 without, and two edges in turn 12 against 13; churned regions
  settled +5.0% over the cheapest with it against +5.4% without. With equal
  uplinks it buys nothing: every edge of a region ranks the two cores by the
  same rendezvous hash per broadcast, so every edge's path join lands on the
  same core. Ranked readers measured 29 and 13 there too.
- **What it costs:** a request and an answer on the wire, and exactly 1.0
  warm message per reader change on an unpeered CDN against none. The edge
  holds one answer per extra upstream per demanded Instance; the upstream
  holds nothing and carries no traffic.
- **Under ranked readers** an answer carries the upstream's rank besides the
  edge, and joins the edge's options like an advert: taken only from a lower
  rank. It was measured only under settled pulls.
- **Until then** an operator gets the same tree by pricing each edge's
  uplinks equally.
- **Its condition:** add it once the simulator measures it under ranked
  readers, the settling model holds with the query in it, the edge's bound of
  one answer per extra upstream per demanded Instance, and none on the
  upstream, is enforced and tested, and a deployment has unequal uplinks
  where the core-mesh copy it saves matters.

### What each part costs

| part | on the wire | relay state | bounded by | without it |
|---|---|---|---|---|
| advert | offer, path, mark, and rank on the announce stream, in order with the route messages, and its withdrawal | one heard per neighbour per Instance the relay has a route for | neighbours; the path by the hop limit | path joins: drones in turn 5 uplinks against 1, a ring 48 against 8, tiers 1191 against 1095 |
| the rank | a route cost and a Node ID on each SUBSCRIBE, advert, and subscribe answer, and an update message for a subscription's rank | one per served subscription and per heard advert or answer, and the rank last told on each pull | as the adverts | the rule never settles (`Orderless.cfg`); without moving for price at all, settled pulls drift +3.4% / +16.2% / +5.4% |
| the route layer's mark on adverts | one bit on the advert, the answer, and the query's answer, as a route's offer carries | the bit with each heard advert and live pull | one per offer | an edge carries a copy between its cores, which no route would: 51 readers in 400 churned regions fed that way under the pick |
| offer on the subscribe answer | a cost, path, mark, and rank on the answer, and an update on each change; none when the parent lost its upstream | one per pull | one per subscription | a relay cannot keep its current parent by its answer once that parent stops advertising to it, and a lost parent cannot say so |
| path in the offer | a list on the advert, the answer, and the query's answer | as the offer | the hop limit | under ranked readers no mesh went unsettled, but a pull cycle formed on stale adverts lasted up to 99 deliveries on drones against 29; under settled pulls 2 of 6,000 churned meshes never settled |
| query, the later step | a request naming an Instance; one answer, offer, path, mark, and rank or none | the edge keeps one answer per extra upstream per demanded Instance until judged; the upstream none | upstreams beyond the first | an edge whose cheaper uplink is cold: 29 against 24 |

What the pick needed and ranked readers delete: the rule against a warm
signal from a relay on the taker's own path, since a neighbour pulling
through a relay never feeds a rank below that relay's; and the rule that a
move under way stays while no dearer. Make before break stays for every
move of a live pull. A lost pull carries nothing, so a new one replaces it
outright.

### What a relay sends

- **The rank.** A relay with a reader of its own for an Instance ranks it
  `(route cost, Node ID)`, the route layer's summed cold cost to the
  Instance; it changes with the route. Each SUBSCRIBE carries the lowest rank
  among the readers it feeds, the relay's own and each downstream pull's,
  and an update message when that changes. A relay with no ranked reader
  downstream sends none.
- **The offer.** What a relay R with a live upstream through parent P offers
  neighbour N: `O(R→N) = O(P→R) + warm(P, R)`. The publisher's relay offers
  0. The offer prices the advert audience below; the choice prices a copy at
  the link alone.
- **The path and the mark.** Every offer names the relays the data passes
  from the publisher's relay to the sender, and carries the mark: set once
  the copy came down a link its receiver marked upstream anywhere on that
  path, and kept on every hop after, as a route's mark is.
- **Adverts.** While R has demand for an Instance besides N, N has not
  marked R upstream, and R has not both marked N upstream and a copy whose
  mark is set, it tells N its offer and the lowest rank it feeds besides N on
  the announce stream when `O(R→N) + cold(R, N)` is below N's route cost for
  the Instance, or equals it through a next hop other than R. It tells again
  on each change, and withdraws when the demand, its upstream, or that test
  ends. Adverts keep their order with the route layer's messages on the same
  announce stream. The audience is priced from the publisher, unlike the
  choice, so a warm core never tells another core whose own route is
  cheaper. R knows N's route cost from what N announces it, and computes it
  when N's route runs through R. The mark is the route layer's own rule:
  without it an edge advertised one core's copy to its other core, which
  then pulled through the edge, a path no route offers.
- **Answers.** The subscribe answer carries the parent's offer, path, mark,
  and the lowest rank it feeds besides the subscriber, an update on each
  change, and none once the parent lost its own upstream, which it says at
  once to a new subscriber too. A subscriber waits for that answer before it
  counts the pull as live.
- **The query, the later step.** A relay with more than one upstream sends
  each upstream besides the one its path join takes one query when it gains
  demand: one request, one answer. The upstream answers its warm offer and
  rank, or that it has none, from what it holds, and keeps nothing: it
  forwards nothing, holds no demand for it, and sends no update. It is not a
  metadata-only subscription: one forwarded by a cold core would hold demand
  of a kind across the core mesh for as long as the viewer stays, the
  standby the epic keeps opt in and off inside the CDN.

### What a relay chooses

- **Its rank** is the lowest rank it feeds: its own reader's, and each
  subscription it serves.
- **Its options** are its route, priced at the route layer's summed cold
  cost, and each warm copy it has heard of, by advert or by its current
  parent's answer, whose rank is strictly below its own, priced at the
  link's cold price alone: that copy is fed whatever this relay does. A copy
  whose path names the relay is never an option.
- **It heads for the cheapest option** whenever its options or its rank
  change: at a join, on an advert, answer, or rank update, and on a route
  change. Equal options fall to a warm copy before the route, then the
  rendezvous hash, as `route_order` in `rs/moq-net/src/model/origin.rs`
  already ranks equal routes. A live pull moves make before break: the old
  pull stays until the new one is answered live. There is no hysteresis and
  no timer.
- **When its parent answers none, or its pull's path names it**, it
  replaces the pull outright with a fresh choice. A pending pull whose
  target is no longer the best is replaced: two relays that each turned to
  the other on stale adverts otherwise wait on each other for good.
- **A subscription with no rank.** From an endpoint it is a reader of the
  relay it reaches, which ranks it as its own. From a relay, one on a version
  before the warm wire, it pins the relay it reaches to its route for that
  Instance while it lasts: nothing carries a rank or a path through the older
  relay, so no rule here can say a warm copy does not loop back through it.
  The older relay sends no advert, so its copy is never taken.
  The spike never had such a relay in a mesh, and its simulator does not
  build this rule: `WarmSim::rank` drops a pull with no rank, so a relay
  feeding an older relay and a ranked reader of its own would still take a
  warm copy. Build the rule above, and test the mix as below.

### Simultaneous starts

Drones, rings, and edges that start at once merge onto one uplink: the
higher rank moves onto the lower rank's copy once it hears the advert (19
against 51 on drones, 112 against 392 on the ring). That is the merge the
maintainer's rule of 2026-11-15 asked for, which under the pick's adverts
added cycles (18 deliveries through merged pulls in 400 churned drone
meshes); ranked readers merge with no cycle, the same unfed reader-hops (15
on drones at once), and 64 messages against 32. A drone that loses the
uplink hands it to one other drone, not four (94 against settled pulls'
118).

### What holds

- A warm link costs its warm price and a cold one its cold price, both at
  least 1, summed from the source along the path the data takes.
- Extend today's selection rather than adding a second selector. A front
  already moves to a better route of its own Instance and resplices
  (`rs/moq-net/src/model/front.rs`); here it does so on the events above. A
  front moves only to a copy of its own Instance.
- The rule runs in production code that the fixtures and sweeps drive. The
  stripped rule lived only in a test simulator that checked it against
  itself, so a rule that never offered a copy passed. Port the warmth shapes
  and the churn sweep onto production relays: pops, drones in turn and at
  once, the lost uplink, the ring, the tiers, and random churned mesh 503.
- Port the rule the `verify-warm` model checks as `Rule = "order"`, move its
  Coverage table from the simulator onto the port, and keep the model in
  step with it. The query is enabled only once the model holds with it.
- Close the model bound the spike left open: a reader change during an
  uplink flap, where a rank changes while readers move. On the three drones
  it passed 2M states in about an hour without finishing, so the spike
  checks the flap with fixed readers only (`Flap.cfg`). Get it to finish, by
  a smaller mesh (two drones and the publisher's relay) or a symmetry
  reduction, or say why it cannot and what covers it instead.
- Put a relay that sends no rank, path, or advert in the churn sweep and the
  model, beside ranked relays and with a ranked reader on the relay it pulls
  through: the mix a rollout with two releases side by side produces. The
  rule above pins that relay to its route. Counting the older relay's pull
  as a reader of the relay it reaches, as an endpoint's is, would keep warmth
  for the pinned relay's own readers; take it only if the sweep and the
  model hold with it.
- Cover independent delivery, which the model's one register per neighbour
  cannot: a neighbour's new rank arriving before or after its advert and
  its answer, a stale advert outliving a rank's withdrawal or replacement
  included. Split the register in the model, or pin the cases in the port's
  acceptance tests with each stream delivering at its own pace.
- Measure what the moves cost readers before reading the simulator's unfed
  figure as production behaviour. Under churn ranked readers leave 15% to
  25% more reader-hops unfed than settled pulls in the simulator, yet readers
  already fed lose nothing: their unfed hops are equal or fewer (5,388
  against 5,443 on random meshes, 1,977 against 2,098 on drones), since a
  move is make before break. The whole rise is readers waiting for their
  first frame (18,307 against 15,033 on random meshes, 13,477 against
  10,273 on drones). Their subscribe and answer wait behind the rank and
  advert messages the rule adds, because the simulator delivers one control
  message per step across the whole mesh: unfed hops per delivery match
  across arms (0.319 against 0.317 on random meshes, 0.174 against 0.160 on
  drones, 0.304 against 0.316 on regions). Keeping a pending pull while its
  target stayed an option changed nothing (21,809 against 21,917 on random
  meshes). The
  acceptance tests measure unfed reader-hops and first frames with each link
  delivering in order at its own pace, beside path joins.
- Bound the warm state a peer can make a relay hold: one advert per
  neighbour per Instance the relay has a route for, a path no longer than the
  route layer's hop limit, one rank per served subscription and heard
  advert, and adverts for anything else dropped. Adverts are per broadcast
  like the claims the announce stream already carries over every link, so a
  relay hears at most one per neighbour per demanded broadcast. A query
  leaves nothing on the upstream; the edge keeps at most one answer per
  upstream beyond the first per demanded broadcast.
- Check the Goal's efficiency against plain path routing on the same shapes,
  as the table above does: on the simulator, 4.3 to 13.8 messages per reader
  change against 2 to 5.4, apart from the lost uplink, and under churn the
  trees settled on cost 1.2%, 4.9%, and 4.4% over the cheapest for the same
  readers on random meshes, drones, and regions, against path joins' 2.7%,
  28.9%, and 5.0%.
- Port the growth sweep, and hold its laws, per link kind. Per reader change,
  warm messages are flat in the number of demanded broadcasts, and trees and
  first frames match settled pulls' in every growth row.
  - Between cores, a full peer mesh: flat in cores. A core with demand is
    never cheaper for another core than that core's own route, so it never
    advertises to one. An unpeered CDN sends none for one viewer and 0.67 per
    reader change for a crowd, the rank updates on subscriptions it already
    holds, from 4 to 32 cores.
  - Down a marked link: no advert. Up it: no advert of a copy that came down
    another. With the query, one per upstream beyond the first, with its
    answer, when an edge gains demand.
  - Between edges or drones that peer: about 2.5 n per crowd reader change
    (161 at 64 drones, against settled pulls' 63), since every one could
    take the copy and every rank change reaches it; that is what buys the
    cheapest tree there.
- A region's edges do not peer every pair as it grows: they reach each other
  through the region's cores. Peered, the region is a drone mesh and pays
  its per-peer cost (165 per crowd reader change at 64 edges per core); the
  tree it buys there, 288 against 544 unpeered for a crowd, comes from
  pricing an edge-to-edge link at a third of an uplink.
- Port the cases as acceptance tests, with drones, rings, and edges starting
  at once, which ranked readers merge, and the same sweeps with each link
  delivering at its own pace. A first frame matched the path join's on every
  shape measured except drones one hop apart (81 ms against 69), where the
  second drone first pulls its uplink and then moves onto the first.
- Export upstream copies held, choices by cause (join, advert or answer,
  rank change, route change, parent lost its upstream, and query answer once
  it lands), moves, and adverts and rank updates sent and held, through the
  relay's existing metrics.
- `rs/moq-net/AGENTS.md` says a relay behaves the same warm or cold. That
  covers answers to requests; this changes which neighbour a request goes
  to. Bring a rewording to the maintainer if the two contradict.

Public API: report any relay configuration added, such as a warm cost per
link. Wire, on lite-07-wip or the first wip version after the cut as
decided 2026-11-19, and all additive: an advert on the announce stream carrying an Instance's offer,
path, mark, and rank, and its withdrawal, in order with the route messages
there; a rank on the SUBSCRIBE and a message updating it; the same offer,
path, mark, and rank on the subscribe answer and its updates; and, with the
later step, a query naming an Instance, answered once with an offer, path,
mark, and rank or none. A rank is a route cost and a Node ID. Nothing
lite-07 freezes changes.

## Waiting

- Maintainer: whether to build the port. Its saving on the deployment's graph depends on a warm discount that trades reader latency for backbone: 20% of backbone link-groups at +7 to 21% mean reader latency with a warm price at a third of the cold, and 2% to 4% without one. Options: (1) adopt latency link costs on the deployment first, which needs no change here, and build the port after the lite-07 cut only if that latency trade is accepted, its wire then in the first wip version after the cut, recommended, since the costs alone remove a 5 to 12% reader-latency stretch and the port's saving is the trade itself; (2) build the port now for lite-07, at the risk of rushing an XL wire into the cut for a trade not yet accepted; (3) drop the port. An agent cannot take the step: it is a product trade between reader latency and backbone cost, and no check or measurement decides it. Done when the answer is recorded in this Plan.

## Related

- [Real relays replay a mesh and its faults](/quest/m2/multicast/mesh-replay.md) - the port runs through it before its wire may join lite-07
- [Route health](/quest/m3/route-health/README.md) - its metadata-only subscription answers the same way
