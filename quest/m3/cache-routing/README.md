# Cache-aware routing

## Goal

A relay stays on a broadcast's path only while it has a local reader, feeds
two or more downstream sessions, or is its one consumer's cheapest route
given the copies the others hold. Otherwise its child bypasses it, so a broadcast with one viewer
crosses fewer relays. A relay joins a neighbor
that already carries a broadcast instead of pulling a second copy, so a LAN
of relays with their own uplinks pulls one copy over one uplink. Routing
stays loop-free under stale state, converges once demand and topology stop
changing, and sends no per-broadcast message for an idle broadcast.

Non-goals: steering clients to the relay that holds a broadcast; decisions
from measured load or RTT ([Link quality](/quest/m5/link-quality.md));
browsers serving copies they receive; a relay on neither consumer's cheapest
route becoming a shared stop; one copy across a multi-hop sparse overlay
rather than among direct neighbors; and creating links, since routing
chooses only among the sessions an operator configured.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan):

1. ✅ **Node routes are the cold cost, and optional per-broadcast warm routes
   are the warm cost.** The per-origin ROUTE metric of
   [Cluster routing](/quest/m2/cluster-routing/README.md) stays the cold
   cost, static and one per origin. A relay carrying a broadcast may
   advertise a warm route for it. This revises Cluster routing's non-goal of
   cache-dependent route prices. (rejected: per-broadcast forwarding with a
   cache-only subscribe and no advertisement; keeping routing cache-agnostic)
2. ✅ **A warm route is per neighbor and travels one hop.** Reopened by
   item 13; replaced by item 14, where a warm route propagates and a relay
   offers each neighbor the best copy whose Hops name neither. It went only to
   direct neighbors and was never forwarded. B was warm toward N only while B
   had demand besides N's (a local reader or another downstream session),
   and withdraws once that demand has been gone for the copy's existing
   linger, with no new timer. B sends it only to neighbors whose cold metric
   for the origin is at least its own, since no other neighbor may use it
   (item 4). Without the per-neighbor rule, a relay whose reader left stays
   warm because it still feeds N, and N never skips it again. (rejected:
   broadcast-wide warmth, as the pre-#3225 design had; forwarding warmth k
   hops, which floods every warm transition)
3. ✅ **One cost per link, priced marginally.** Replaced by item 14: a copy
   ranks as a route does, drain then priority then cost, and its carrier offers
   it at no cost, so joining a copy costs only the links it crosses. Pulling through a warm
   neighbor costs the link alone, since that neighbor's cold metric is
   already paid; a cold neighbor costs the link plus its cold metric. A tie
   goes to the warm route, then to item 4's hash. A topology that wants both
   behaviors prices a link that skips one relay at more than one neighbor
   link and less than two: a cold broadcast then takes the skip, and once
   the skipped relay pulls for its own reader, its neighbor joins it. With
   uniform prices both cases tie and fall to the hash. (rejected: a warm and
   a cold price per link; a warm price the carrier seeds)
4. **Eligibility, which the proof must hold.** Reopened by item 13. A relay
   may pull broadcast P
   through neighbor N only when N's advertised route to the request's pinned
   origin has a newer seqno, or an equal seqno with
   `(cold metric, hash(P, N))` below `(feasibility distance, hash(P, self))`.
   A feasibility distance never exceeds what its node advertised at that
   seqno, so it strictly decreases along every forwarding edge even when an
   advertisement is stale: no loop, no rank, no hold timer. The pre-#3225
   rank was a cold cost each relay reported about itself, so a stale report
   could let a ring of relays all let go at once, which its 500 ms hold only
   narrowed. Route advertisements stay one selection per origin.
5. ✅ **Proof first:** model and sweeps before the implementation.
   (rejected: implementing with targeted fixtures only)
6. ✅ **The proof picks the wire placement.** The warm message is designed
   fresh rather than reviving ANNOUNCE_TAKEOVER, which
   [moq-dev/moq#8063](https://github.com/moq-dev/moq/pull/8063) deleted in
   favor of instance continuity. The proof sizes it and
   picks lite-07 or an additive SETUP extension. (rejected:
   reusing ANNOUNCE_TAKEOVER's slot)
7. ✅ m3, after the m2 route layer it ranks on. (rejected: m2; m4) Amended
   2026-11-09 (maintainer, /quest-plan from Kixel's audit of its fleet plan):
   ✅ the proof alone moves to m2, ahead of the lite-07 cut, and lands the
   message's draft section and codec there if it places it in lite-07
   (rejected: a design-only sizing quest in m2; leaving the proof in m3 and
   accepting a SETUP extension). The implementation stays in m3. This
   replaces item 6's rule that choosing lite-07 went back to the maintainer;
   item 11 records the placement the maintainer chose.

Proof results 2026-11-10 ([moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457)),
modeled in `rs/moq-net/src/route/tests/cache.rs` over the real `Router`:

8. **Item 4 holds.** No sweep step in 3000 churned scenarios formed a
   per-broadcast cycle, and the fixtures fail when either the feasibility
   check or the hash order is dropped. The router answers it with the
   crate-private `Router::eligible`, which marks a route tied at the
   feasibility distance so the caller applies the hash. The argument needs a
   feasibility distance that never rises while a neighbor holds the advert;
   the overtaking echo that
   [moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447) found breaks
   that, which [the implementation](/quest/m3/cache-routing/select.md) must
   not ship before it is fixed.
Decided 2026-11-10 (maintainer, /quest-iterate on
[moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457)), from the
proof's proposals:

9. ✅ **Each pull names whom it serves**, amending item 2; replaced by item 14,
   whose SUBSCRIBE says only whether it joins a copy, and lite-07 no longer
   carries `Serves`: a trailing `Serves` field on SUBSCRIBE and SUBSCRIBE_UPDATE. (rejected: no field)
   "Demand besides N's" can't be told from one hop: if B's only demand is a
   relay pulling on N's behalf, B counts warm toward N, N joins B, B's demand
   vanishes, and the cycle repeats every linger. `Serves` is alone (a local
   reader, or two or more roots below it), a single root relay's Node ID, or
   unknown (an endpoint that doesn't say, which warms nothing). N counts B
   warm only when B serves alone or a root other than N and N's own root. A
   narrowed warm route stands until the copy's linger runs out, as the
   withdrawal already does.
10. ✅ **Price a dependent copy at its cost, and move only on strict
    improvement**, amending item 3; replaced by item 14, whose offers do not
    depend on who pulls what, and whose sweep settles every scenario with no
    `UNSETTLED` list; the Goal's "cheapest cold route" became
    "cheapest route given the copies the others hold". (rejected: dropping
    mutual warmth so only a relay with a local reader advertises, which
    converges trivially but loses the LAN sharing that feeding two sessions
    gives; link-only pricing) Link-only pricing has no steady state when two
    relays each keep the other warm (sweep scenario 2062 flips forever). A
    warm route carries its sender's own price for its pull; a receiver the
    copy depends on (its serves names only the receiver's root) pays link
    plus that cost, an independent one pays the link alone, and a relay
    moves only for a strictly lower `(draining, price)`. With exact
    information that is a potential game on the forest's total link cost.
    Under the sweep's churn, with the draft's Rendezvous Hash, 2999 of 3000
    scenarios settle; scenario 2251 never does, as two carriers keep a
    narrowed route standing while two relays swap between them. The sweep names them in
    `UNSETTLED` and checks each still never settles; item 14's rule settles
    them.
11. ✅ **The wire is in moq-lite-07 itself** (its warm fields and `Serves`
    replaced by item 14's), overturning the proof's choice
    of a SETUP extension. (rejected: an additive Warm Routes SETUP
    extension) Lite-07 is unpublished, so its shape can still change until
    the cut, and a later change goes to the next version. The draft's
    Warm Routes section and the Rust codec landed with the proof:
    `WARM_START` (0xC) carries the Route ID it travels under, the path
    suffix, the Instance and Minted encoded as in ANNOUNCE_START, then the
    serves and cost, and assigns a Warm ID; `WARM_UPDATE` (0xD) replaces a
    warm route's serves and cost by Warm ID, so a price change costs one
    message rather than an end and a start; `WARM_END` (0xE) withdraws it.
    A warm route follows the ROUTE_START it names on the announce stream the
    carrier answers, and a ROUTE_END retires the warm routes under it.
    SUBSCRIBE and SUBSCRIBE_UPDATE end with `Serves`: a type of unknown
    (0x0), alone (0x1), or relay (0x2) followed by its Node ID. The hash is
    the Resolution section's FNV-1a rendezvous hash of the path and the
    neighbor's Node ID. Until the implementation, Rust reads warm routes and
    serves and ignores them, and says unknown; `js/net` says alone, since it
    forwards nothing, and skips the warm types as unknown, which is how an
    endpoint that does not choose per broadcast ignores them. About 4.5 warm
    messages per reader change across the sweep's churn, which also flaps
    links and advances seqnos; an idle broadcast sends none.

Measured by the proof:

12. **Costs the sweeps measured.** A full LAN mesh of 6 relays pulls one
    uplink copy per broadcast; a ring of 6 keeps 12 copies for 8 broadcasts,
    one per hash local minimum. An origin seqno advance that reaches a child
    before its warm parent makes the parent's older route ineligible, so the
    child moves off and back (2 moves in the chain fixture); origins advance
    on every announce stream open and close. Item 13's paths drop the seqno
    ([routes epic](/quest/m2/cluster-routes/README.md) Plan item 27),
    and this churn with it.

13. Reopened 2026-11-11 (maintainer, /quest-plan on the route redesign):
    items 2 and 4. In the maintainer's words: "I think we need warm routes
    propagating, that decision was made based on this flawed setup." ✅ **A
    warm route is per broadcast and propagates**, for as long as it is
    cheaper than the route its publisher's broadcasts share
    ([epic](/quest/m2/cluster-routes/README.md) Plan item 26). A
    broadcast no relay carries rides the shared route and costs no message of
    its own. Item 4 rests safety on Babel's distance, which cannot let a relay
    join a copy at a neighbour that is no closer to the publisher: the join
    the Goal wants, and why item 2 stopped at one hop. Items 1 and 3 stand.
    Decided 2026-11-11 (maintainer, on
    [moq-dev/moq#8533](https://github.com/moq-dev/moq/pull/8533)): what keeps a warm
    route free of loops and stale copies is its path, as for every route
    (routes epic Plan item 27), and the proof then holds that rule. The
    prototype in `rs/moq-net/src/route/tests/copies.rs` showed three things
    the proof starts from. A warm route carries the path its copy took and
    names the relay that carries it, and a relay refuses one whose path names
    it, so it joins a copy at a neighbour farther from the publisher than
    itself. Two relays that start carrying in the same instant trade copies
    for good unless they are ranked, so a relay already pulling moves onto
    another's copy only if that carrier ranks below it. And when a carrier's
    uplink closes, it and a relay pulling from it point at each other until
    the news crosses their link.
    [moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457) proves items
    2 and 4 as first decided.

14. **The proven rule** ([moq-dev/moq#8732](https://github.com/moq-dev/moq/pull/8732)),
    proven in `rs/moq-net/src/route/tests/cache.rs` over `route::path::Router`
    ([moq-dev/moq#8622](https://github.com/moq-dev/moq/pull/8622)), in three
    layers that each settle before the next depends on them:
    - **Routes** are item 27's, which copies never change.
    - **Offers.** A relay with a local reader is its copy's carrier and offers
      every neighbor its copy, its own Hop ID the only hop, at no cost. A relay
      with no reader offers each neighbor the best of its route and the copies
      offered to it whose Hops name neither relay, when that best is a copy.
      Copies rank as routes do, drain then priority then cost, so an offer
      depends only on readers, routes, and other offers, and settles as a
      monotone path vector does.
    - **Pulls.** A carrier pulls through its best source among its route and
      the copies of carriers ranked below it by the Rendezvous Hash. A relay
      with no reader pulls through its best source whose Hops name no
      neighbor it serves, so long as that is, for each such neighbor, the copy
      it offered it or a lower-ranked carrier's; otherwise it follows its
      route. SUBSCRIBE's `Warm` says whether a pull joins the copy offered or
      follows the route, and a relay pulled along its route follows its own.

    Each join gets a copy whose carrier, then whose rank, is strictly better
    than what it gives the relays joined to it, so once the offers and
    subscriptions in flight are delivered the pulls are free of loops, as the
    maintainer accepted on 2026-11-13. Unranked, two carriers join each other
    and neither is fed. This replaces the rules of
    [moq-dev/moq#8615](https://github.com/moq-dev/moq/pull/8615) (monotone
    copy costs, carrier rank on every move, a relay bound by the carriers
    behind it), which failed a few scenarios per seed because what a relay
    offered depended on who pulled through it.

    Measured by `sweep_cache_routing_long` (3000 scenarios each with and
    without held links, 6 relays, 2 publishers, priorities and drains), under
    12 seeds (72,000 scenarios): every scenario settles on the offers computed
    centrally, an idle broadcast sends no warm message, and about 3.1 warm
    messages go out per reader change. A pull lingering with no demand can hold
    a copy its parent promised another neighbour, so the pulls around it may
    move until the linger (30 s) runs out; settling counts from there. During
    churn a broadcast's pulls form a cycle after 3.3% of steps, in about half
    the scenarios, each gone within 142 deliveries. The sweep runs nightly
    from `test/route-sweep.sh`.

    The wire: WARM_START and WARM_UPDATE carry a copy's Warm Cost, a Flags
    byte whose low bit marks a draining copy, its priority, and its Hops,
    carrier first and the sender last. SUBSCRIBE and SUBSCRIBE_UPDATE end with
    a `Warm` byte. Rust's codec reads and writes both, and `js/net` the `Warm`
    byte, which both send as 0 (follow the route) and ignore when read, as
    `js/net` skips the warm messages; [the implementation](/quest/m3/cache-routing/select.md)
    acts on them.

Prior art: warm/cold route prices
([moq-dev/moq#2925](https://github.com/moq-dev/moq/pull/2925)) were deleted
by prefix routes ([moq-dev/moq#3225](https://github.com/moq-dev/moq/pull/3225)),
and the pop-skipping questline (warm advertise, rank, peer reconfigure) was
pruned in [moq-dev/moq#4393](https://github.com/moq-dev/moq/pull/4393).
Related designs: PIM-SM's switch from a shared tree to a shortest-path tree
([RFC 7761](https://www.rfc-editor.org/rfc/rfc7761.html)), NHRP shortcuts
([RFC 2332](https://www.rfc-editor.org/rfc/rfc2332.html)), and Babel's
feasible successors ([RFC 8966](https://www.rfc-editor.org/rfc/rfc8966.html)).

This README's own work, once its children land:

- End-to-end tests over real sessions, time mocked. A chain of three relays
  plus a skip link, priced 3 and 5: a cold broadcast takes the skip, a reader
  on the skipped relay makes the far relay join it, and that reader leaving
  sends it back to the skip. Two LAN relays with their own upstream links and
  readers on both pull one copy over one uplink, with the owner spread across
  broadcasts by the hash.
- Docs: warm routes and how to price skip links in `doc/bin/relay/cluster.md`,
  and a section on the routing concept page once the
  [routing guide](/quest/m1/routing-guide.md) has added it.

## Related

- [Cluster routing](/quest/m2/cluster-routing/README.md) - the node routes and feasibility distances this ranks on
- [Relays split horizon per session](/quest/m3/route-split-horizon.md) - per-session suppression of node routes, beside warm routes' per-neighbor rule
