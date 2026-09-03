# Cache-aware PoP skipping

## Goal

Give an unpopular broadcast a short cold path without sacrificing the backhaul
deduplication a sparse mesh gets once the broadcast is warm. Every relay connects
to all healthy relays in its own PoP, its base-graph neighbor PoPs, and the PoPs at
graph distance two. A cold subscriber uses the direct distance-two session rather
than forwarding through an idle intermediate; a relay with any warm track advertises
the cheaper route, pulling later subscribers back onto the copy the cluster already
has. Full eligible-PoP pairing is accepted for now; on the topology this was
designed against it takes live persistent relay sessions from 29 to 78 (of 120 for
a full mesh).

For `sjc0 -> dal0 -> iad0`, with the publisher in IAD:

1. Cold SJC sees direct `sjc0 -> iad0` at 5 and `sjc0 -> dal0 -> iad0` at 6, so it
   takes the direct session on price rather than on a tie-break.
2. Another SJC relay reaches that warm copy over the same-PoP link for 1, against 5
   to open its own.
3. A DAL subscriber initially pulls directly from IAD at cost 3. DAL then ranks
   ahead of SJC because its cold route is cheaper (3 against 5), so SJC migrates at
   a group boundary and both share DAL's one IAD pull.
4. SEA makes the same local decision and can join the already-warm aggregation
   tree rather than opening another copy from IAD.

## Plan

### Where this stands after prefix routes

[moq#3225](https://github.com/moq-dev/moq/pull/3225) made an announcement a
route over a path *prefix* and deleted the machinery this questline had
already landed: the warm-cost discount, `COST_LINGER`, the
`(cold, hash)` adoption gate, the handover hold, and the per-broadcast front
that hosted all of it. A relay now forwards accumulated costs only.

What survives is the part that was expensive to get right:

- `origin::Cost { warm, cold }` on the wire, decoded on lite-06, with
  `Cost::UNKNOWN` reading an inexpressible cold as the ceiling rather than as
  free.
- `route_order`, which still breaks a warm tie on the lower cold cost ahead of
  hop count.
- `DRAIN_COST`, and the hop list as the loop check.
- The link-price decisions below, which were rulings about the topology rather
  than about the code that read them.

So this questline is no longer "add a rank to a working discount". It is
re-deriving warmth on a route model that prices prefixes, then putting the
adoption gate back on top.

### The tension prefix routes introduce

Warmth is a property of one broadcast. A route covers a prefix, which is a
claim about a set of paths. A relay carrying `pid/foo.hang` knows nothing about
the rest of `pid/`, so there is no honest way to discount the prefix route it
already advertises: doing so would attract subscribers for every cold path
underneath it.

### Decisions

- **A carrying relay advertises the exact broadcast path as its own route,
  priced warm.** Per-broadcast warmth becomes a more specific route rather than
  a discount on a broader one. Nothing new goes on the wire, and the existing
  selection rule already prefers it.
- **Selection keeps specificity ahead of cost.** `best_server` filters to the
  longest covering prefix and only then orders by `route_order`, and the lite
  draft says the same. This matches longest-prefix-match everywhere it appears
  (IP forwarding, BGP, DNS closest encloser, URL routers), and for the same
  reason: routes of different prefix length describe different destination
  sets, so comparing their costs asks "what does this broadcast cost" against
  "what would anything under here cost". Cost decides between routes that cover
  the same thing, which is exactly where the warm/cold pair was designed to
  work.
- **A draining carrier retracts its exact-path route rather than repricing
  it.** Under specificity-first, forgoing the discount is not enough: a route
  priced at the ceiling still wins on specificity and keeps attracting
  subscribers a drain is trying to move. Retraction drops the claim, and the
  broader route the content is still reachable through takes over. This
  replaces the draft's ceiling-exemption paragraph, which was written when the
  discount rode a single per-broadcast advertisement.
- **Adoption and resume need different identities.** Adoption keys on the
  *last* hop of a route, the peer that advertised it and the parent a relay
  would be adopting; resuming a subscription keys on the *first* hop, who
  produced the content, which alternate routes to one publisher deliberately
  share. The pre-#3225 front kept both (`FrontState.publisher` for the first,
  `handover_allowed` and the hold for the last), with `same_identity` comparing
  either and refusing `Hop::UNKNOWN` on both. [moq#3312](https://github.com/moq-dev/moq/pull/3312) restored that comparison rule and the
  publisher half as the first-hop resume; [Rank](/quest/m2/pop-skipping/rank.md) owns
  the carrier half rather than reusing the wrong one.
- The operator hand-authors one undirected base graph. Same-PoP connectivity is
  unconditional, not a self-edge operators must remember. The radius-two closure
  and shortest base-graph distance are derived and validated.
- The initial reference link costs are local 1, base neighbor 3, and distance-two
  skip 5. The invariant that buys PoP skipping is `skip < 2 * neighbor`: a cold
  skip must beat the equivalent two-edge path outright, so 5 against 6 works while
  anything from 6 up preserves the old cold path and defeats this quest. Keeping
  the skip strictly below rather than equal to the two-edge path means the choice
  is made on price, not on the hop-count tie-break below it.
- No link is free, including a same-PoP one. A local transfer still costs a NIC, a
  hop of latency, and a copy; what is genuinely free is bytes already flowing, and
  that is the warm route's job, not the link price's. A floor of 1 also prices
  chain *length*, so a PoP converges on a flat tree around one puller instead of
  daisy-chaining at no cost, and it means adopting a parent strictly increases the
  adopter's own cold cost, which leaves the per-broadcast hash as a tie-break
  between equally-placed relays rather than the only thing keeping the order
  strict.
- Warmth is broadcast-wide: demand for any track makes the broadcast warm. When
  demand drains, the exact-path route follows the existing 30-second spliced-track
  lifecycle (`TRACK_IDLE_LINGER`) while at least one track copy remains retained.
  Do not add a second timer with a different definition; `COST_LINGER` was that
  timer and is already gone. A retained track has canceled its upstream
  subscription, so "warm" here intentionally means reusable route/track state plus
  hysteresis, not that every future byte is already in memory.
- Provider economics are directional and dominate locality in a mixed-provider
  deployment. Conceptually the metric is `(serving provider egress class,
  topology distance)`: pulling from an unmetered relay can be cheaper than the
  reverse direction, while equal provider classes retain the 1/3/5 locality
  order. Keep these components structured until the final peer cost is encoded;
  choose an encoding whose economic stride exceeds the maximum accumulated
  topology distance allowed by the bounded hop list, which at a 32-entry chain
  and a 5-cost worst link is 160.
- Cluster sessions use `moq-lite-06-wip`, explicitly. Lite05 silently drops
  the cost; the MoQT Cluster extension is not the chosen cluster wire for this
  questline.
- Fleet rollout stays downstream: moq.pro owns rendering the priced radius-two
  topology, the two-phase Lite06 cluster cutover, and the staging and live
  route-verification proofs. This questline completes when the mechanism above
  lands here.

### Peer reconfiguration

The URL-backed half is complete in
[moq#2874](https://github.com/moq-dev/moq/pull/2874): canonical identity stays
separate from dial configuration, so changing `?cost=` or an inline credential
replaces the active session while an identical render is a no-op. It also
preserves the last-good topology on malformed input, keeps an identical fallback
session alive, redacts credentials from parse errors, and tracks overlapping
gossip paths so an old unannounce cannot stale a replacement. The remaining
boundary is structured policy that does not live in the URL, especially selected
wire version and the two directional costs of one bidirectional session.

## Quests

- [Warm advertise](/quest/m2/pop-skipping/warm-advertise.md) - a carrying relay
  advertises the exact broadcast path as a warm route, and retracts it when
  draining or idle
- [Rank](/quest/m2/pop-skipping/rank.md) - rank warm relay candidates by cold
  cost and adopt only downhill, with a hold that outlasts cost propagation
- [Peer reconfigure](/quest/m2/pop-skipping/peer-reconfigure.md) - reconfigure
  peer policy when negotiated versions or directional costs change, proving
  both sides of an asymmetric link

## Related

- [drain](/quest/m2/drain/README.md) - a second relay per PoP makes the same-PoP link price and its connection cardinality operationally important
- [wildcard](/quest/m2/wildcard/README.md) - it reuses this questline's route cost, and needs a cluster on Lite06
- [relay-memory](/quest/m2/relay-memory/README.md) - a denser mesh multiplies whatever a non-selected route costs
