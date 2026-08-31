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

### Decisions

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
  that is the warm discount's job, not the link price's. A floor of 1 also prices
  chain *length*, so a PoP converges on a flat tree around one puller instead of
  daisy-chaining at no cost, and it means adopting a parent strictly increases the
  adopter's own cold cost, which leaves the per-broadcast hash as a tie-break
  between equally-placed relays rather than the only thing keeping the order
  strict.
- Warmth is broadcast-wide: demand for any track makes the broadcast warm. When
  demand drains, the discount follows the existing 30-second spliced-track warm
  lifecycle (`TRACK_IDLE_LINGER`) while at least one track copy remains retained.
  Remove the separate five-second announcement-only `COST_LINGER`; do not add a
  second timer with a different definition. A retained track has canceled its
  upstream subscription, so "warm" here intentionally means reusable route/track
  state plus hysteresis, not that every future byte is already in memory.
- The hop list remains the steady-state loop detector, but it cannot make two
  simultaneous handovers safe: A and B can each advertise a clean path through IAD,
  then adopt each other before either new hop list arrives. Adoption therefore
  descends a per-broadcast rank `(cold route cost, hash(broadcast path,
  relay origin id))`. Lower cold cost wins; equal-cost peers choose the lowest hash.
  The winner keeps its cold upstream rather than adopting a higher-ranked peer.
  Including the broadcast path distributes ownership instead of making one hot
  relay win every broadcast. Marginal cost remains the primary route metric, and
  where it ties the *lower* cold cost wins, ahead of hop count. Both tie-breaks
  answer the same question, how far away the content is, and cold cost answers it
  in the operator's own prices while hop count only counts relays; the priced
  answer goes first, and a route straight from the publisher can never lose it,
  since at equal marginal cost its cold equals its marginal.
- That rank is only a shared order while the costs behind it are. A relay reports
  its own cold cost, so a report still crossing the mesh can be lower than what its
  sender would say now, and rings of relays can each rank a stale neighbour below
  themselves and all let go at once. Rising costs are the whole hazard: if costs
  only fell, a stale value would only make a peer look worse than it is. A relay
  therefore holds a re-parent onto another relay for ~500ms and re-evaluates when
  the hold expires, rather than acting on the decision that armed it. The hold is
  not there to stagger the relays, which a uniform delay cannot do; it outlasts the
  propagation of the very costs it is deciding on, so the sizing rule is just
  "longer than an announcement crosses the mesh", plus a stable per-relay spread so
  a PoP does not reconsider on one instant. It covers only trading a working
  upstream for a better one: an idle relay is pulling nothing, a one-hop chain is
  the publisher, and leaving a drained or vanished route stays immediate.
- Cluster sessions use `moq-lite-06-wip`, explicitly. Lite05 silently drops
  the cost; the MoQT Cluster extension is not the chosen cluster wire for this
  quest.
- Provider economics are directional and dominate locality in a mixed-provider
  deployment. Conceptually the metric is `(serving provider egress class,
  topology distance)`: pulling from an unmetered relay can be cheaper than the
  reverse direction, while equal provider classes retain the 1/3/5 locality
  order. Keep these components structured until the final peer cost is encoded;
  choose an encoding whose economic stride exceeds the maximum accumulated
  topology distance allowed by the bounded hop list, which at a 32-entry chain
  and a 5-cost worst link is 160.
- Fleet rollout stays downstream: moq.pro owns rendering the priced radius-two
  topology, the two-phase Lite06 cluster cutover, and the staging and live
  route-verification proofs. This questline completes when the mechanism above
  lands here.

### Existing foundation and gaps

moq-relay 0.14.11 / moq-net 0.2.12 (`adad52b2`) already carry cumulative route
costs, `?cost=N` cluster peer URLs, a zero-cost advertisement for a demanded
broadcast, group-boundary route migration without announcement churn, and
hop-list loop rejection. They also carry the simultaneous-warm-peer hash gate.
The missing piece is the rank above: the current gate may keep SJC on IAD based
only on the hash, so it does not guarantee that a cheaper cold parent such as
DAL becomes the aggregation point.

The URL-query reconciliation slice landed in
[moq#2874](https://github.com/moq-dev/moq/pull/2874): canonical identity now stays
separate from dial configuration, so changing `?cost=` or an inline credential
replaces the active session while an identical render is a no-op. It also preserves
the last-good topology on malformed input, keeps an identical fallback session
alive, redacts credentials from parse errors, and tracks overlapping gossip paths
so an old unannounce cannot stale a replacement. The remaining reconciliation
boundary is structured policy that does not live in the URL, especially selected
wire version and the two directional costs of one bidirectional session.

### rank

Extend the Lite06 WIP route advertisement with the cold rank needed beside its
discounted marginal cost. The cold cost is the relay's best path with warm discounts
removed; it remains observable while the serving path is warm. Compute the hash from
the absolute broadcast path and stable relay origin id locally, so only cold cost
needs new wire state. A carrying relay adopts another carrying relay only when the
peer's `(cold cost, hash)` is strictly smaller. Its own rank, not the rank of a parent
it already adopted, is what it advertises, making every warm edge descend and
preventing cycles. The hop list still rejects a stale or malformed route containing
the receiver. Keep marginal cost first in route selection; where it ties, the
lower cold cost wins ahead of hop count, which is the priced version of the same
"how far away is this" question hop count only estimates.

Prove the asymmetric chain (DAL outranks SJC regardless of hash), the equal-cost
race (exactly the lower hash keeps IAD while the other adopts it), a three-node
transitive tree, simultaneous updates, and route loss/reversion. A live track must
migrate only at a group boundary and must not see announcement churn.

### warm-lifecycle

Expose one broadcast-level warm signal derived from all spliced tracks: true while
any track has demand or retains its source copy inside `TRACK_IDLE_LINGER`, false
only after the last copy expires. Make both Lite and IETF publishers consume that
signal even when a cluster selects Lite06, so the model has one semantic. Delete
the five-second cost-only linger and its parallel deadline machinery. Tests cover
two tracks with staggered demand, demand returning during retention, the last warm
track expiring, a route change during retention, and no repeated re-advertisement
after expiry.

### peer-reconfigure

The URL-backed half is complete in
[moq#2874](https://github.com/moq-dev/moq/pull/2874): cost and credential changes
replace the active peer session, identical renders and identical source fallbacks do
not churn it, malformed lists preserve the last-good topology, and overlapping
gossip advertisements cannot tear down the new configuration. Finish the structured
half so an operator's topology renderer can grow provider policy: selected wire
version and each direction's economic cost must participate in reconciliation even
though they are not URL identity. Test both directions of an asymmetric link and a
version-only change; the old session must close once and the replacement must carry
the new values.

## Quests

- [Rank](/quest/m2/pop-skipping/rank.md) - rank warm relay candidates by cold
  cost before hop count, on the Lite06 WIP
- [Warm lifecycle](/quest/m2/pop-skipping/warm-lifecycle.md) - one
  broadcast-wide warmth lifecycle for Lite and IETF publishers, replacing the
  separate five-second cost linger
- [Peer reconfigure](/quest/m2/pop-skipping/peer-reconfigure.md) - reconfigure
  peer policy when negotiated versions or directional costs change, proving
  both sides of an asymmetric link

## Related

- [drain](/quest/m2/drain/README.md) - a second relay per PoP makes the same-PoP link price and its connection cardinality operationally important
- [wildcard](/quest/m2/wildcard/README.md) - it reuses this quest's route cost and rank hash, and needs a cluster on Lite06
