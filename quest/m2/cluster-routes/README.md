# Routes and announces

## Goal

Routing info splits into two layers on the session's announce stream, in
lite-07 (the current wip version, decided 2026-10-05), for every session. A ROUTE advertises reachability of one
origin node; an ANNOUNCE says a prefix lives at a route's node and carries no
path. A link flap or relay loss sends one ROUTE change per origin whose best
route changed, never a re-announce per broadcast; withdrawing an origin-owned
claim does not revive stale copies or end other origins serving the same epoch; failover to another neighbour with
the same epoch preserves continuity; and no stale neighbor copy revives a
claim whose withdrawal is already known.

## Plan

1. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **The draft describes the target before the code switches.** A hop-less ANNOUNCE cannot run before the router drives sessions; the quest's own Plan says so. So the draft now specifies the whole lite-07 route wire, while main's lite-07 session keeps sending hop-based ANNOUNCEs until route session integration ([moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)) lands. lite-07 is unpublished `-wip`, off by default, and the epic lands in one release before the cut. The other option was to put only the new message sections in the draft and leave ANNOUNCE hop-based. I rejected it because ANNOUNCE_FLOOR's "for that route" and the Route ID would then be undefined in the draft.
2. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **The request field is `Node ID (i)`, with 0 meaning "the publisher selects".** It is added to every request that resolves a path: TRACK, TRACKS, SUBSCRIBE, FETCH, and DATAGRAMS. Node ID 0 is reserved everywhere.
3. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **ROUTE_REQUEST names a Node ID, not a Route ID.** A request can then never race a ROUTE_END into a protocol violation.
4. ~~A held ROUTE_UPDATE carries a seqno and down-only bit of 0.~~ Superseded by item 14: a held route is its own message and carries no seqno or bit.
5. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **Floor regions have no order, and a repeated region is a violation.** A sorted order would need Rust's `Path` ordering reproduced exactly in JS. Decoding into a map rejects duplicates at the same cost. Superseded the same day by item 10, which removes regions.
6. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **The Hop setup parameter and ANNOUNCE_OK's Hop ID are dropped from lite-07.** Their only roles were path-vector exclusion and the implicit trailing hop. ANNOUNCE_OK reserves no body for a route-key proof: [moq-dev/moq#7937](https://github.com/moq-dev/moq/pull/7937), accepted the same day, proves the route key once per session in SETUP. The live hop-based codec still switches in session integration, as choice 1 requires.
7. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **An echo naming a Route ID that was never ended, or was already echoed, is a violation at the publisher.** The publisher can check this from its live and unacknowledged ids alone, with no growing state.
8. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **ANNOUNCE_TAKEOVER keeps its shape, with a Route ID in place of hops, and its predecessor must be under the same route.** [moq-dev/moq#8063](https://github.com/moq-dev/moq/pull/8063) deletes the message, so this keeps the change minimal. Decided 2026-11-06 (maintainer, /quest-complete on [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)), asked whether session integration ports the message or lets lite-07 answer a claim as a restart or start until announce-takeover lands: ✅ **port it**, so a job answering a claim stays a takeover across relays. Rejected: accepting the restart and start.
9. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **Unknown announce types stay skipped by length on lite-06+**, as before. An unknown reply type is refused.
10. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **ANNOUNCE_FLOOR is a marker, not a region list:** `{Route ID, Revision}`. It closes a batch and certifies the stream's claims for that route across the whole requested prefix as the origin's at that Revision. A publisher that cannot vouch for the whole requested prefix at one Revision still closes the batch, with a floor that certifies nothing, encoded per item 13. Regions, the replacing list, and the 65,535-byte overflow go away. Accepted cost: a relay with partial knowledge of an origin certifies nothing for that route, so a receiver lets its stale claims go only on ROUTE_END. Rejected: one region per message; a multipart atomic list.
11. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **Clamped claims collapse to the best one.** When several of a route's claims cover a request's prefix (`live` at cost 10 and `live/a` at cost 1, request `live/a/x`), the publisher presents only the most specific, and when it is withdrawn moves the presented claim to the next covering one with ANNOUNCE_UPDATE or ANNOUNCE_RESTART instead of ending it. No wire change; one live claim per prefix per route holds, and a receiver merges claims at the prefix each stream presents them. Rejected: carrying each claim's ancestor depth.
12. ✅ Decided 2026-11-05 (maintainer, /quest-plan): **A floor certifies only what its own request shows.** A stream whose request did not set `Hidden` certifies nothing about hidden claims. No new wire field. Rejected: isolating differently filtered views.
13. ✅ Decided 2026-11-05 (maintainer): **ANNOUNCE_FLOOR encodes `Revision + 1`, with 0 meaning the floor certifies nothing,** the same offset as `Max Age`. Rust decodes it to `Option`. It replaces a Revision of 2^64-1 as the marker: a huge sentinel is a bad pattern and costs 9 bytes as a lite-07 varint. Rejected: a separate message; a boolean field.
14. ✅ Decided 2026-11-05 (maintainer: "use 0 as a sentinel, or make a new message type"; the agent picked the message): **A held route is ROUTE_HOLD (0xB), `{Route ID}`,** replacing a ROUTE_UPDATE with a Metric of 2^64-1. A metric of 0 is real (the origin's own route), so 0 cannot mark held, and offsetting every metric by one would still leave a held update's seqno and down-only bit meaningless and refused. The message carries only what a hold means, item 4's rule goes away, and a metric is now only bounded by overflow, so the wire refuses no metric. The 2^64-1 Seqno and Revision bounds stay: they are bounds, not markers, leaving room for a ROUTE_REQUEST's successor and for the floor's offset.
15. ✅ Decided 2026-11-06 (maintainer, /quest-complete on [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)), asked whether to amend the handoff contract's "a sender that certifies anything for a route answers every stream" or keep it: ✅ **send a route on a stream only while it meets the stream's interest**, with a floor certifying the last withdrawal before the route ends. Withdrawals still END, and what a node keeps per stream it answers is bounded by the origins that stream reads, at a ROUTE_END round trip when an origin leaves an interest. Rejected: keep answering every stream and bound only by per-peer caps, which is simpler but carries the whole node table on every stream. [moq-dev/moq#8293](https://github.com/moq-dev/moq/pull/8293) implements it.
16. ✅ Decided 2026-11-09 (maintainer, /quest-unblock on [moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375)), asked how a token-scoped lite-07 session certifies withdrawals so a clean finish ENDs across a relay: ✅ **the subscriber narrows its ANNOUNCE_REQUEST to its scope's single region** (where its limit and its subscribe grant meet), and reopens the request when that region changes so a widening renewal isn't lost. Floors keep their exact whole-prefix meaning, with no wire change and one SHOULD in the lite-07 draft. A multi-region scope still certifies nothing. Rejected: floors carrying regions on the wire, which reverses item 10; landing with the finish lanes accepting offline.
17. ✅ Decided 2026-11-09 (maintainer, /quest-unblock on [moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375)), asked how item 16's reopen counts the session's announce cap while the old request keeps serving until the new one is SYNCED: ✅ **the old request gets its own budget for the handover**, moving what it holds onto a separate counter with the same cap, so the session's count belongs wholly to the new request. A peer can make the session hold at most twice the cap, and only during one handover. Rejected: dropping the old request at once on every region change, which flickers claims offline; holding a client's first request until its credential is answered and dropping the old one on later renewals.

The items below were decided 2026-11-10 (maintainer, /quest-plan on lite-07 routing), which reopened the route layer's rules after the echo ordering bug.

18. ✅ Asked who is an origin node: **every publisher stays an origin node.** Its identity is for signing, for one node across several links (redundant publishers), and because browsers will forward ([browser mesh](/quest/m5/browser-mesh.md)). Rejected: a relay originating, under its own Node ID, the claims of any peer that sends no Node ID or holds a scoped publish grant, as it already does for lite-06 and moq-transport publishers. That would make route state follow relays rather than publishers and remove qualified Node IDs, the Route Key, and the echo, but a publisher would have no identity of its own. Accepted cost: routes, holds, and seqno requests scale with publishers, so a node must still forget dead identities, and the echo that makes forgetting safe has to be sound.
19. ✅ Asked whether to keep them: **ROUTE_HOLD stays, and the newest floor wins per identity.** Rejected: dropping holds, where each stall takes every claim behind the node offline and re-sends them all; and claims that follow only the selected next hop, which re-pulls a node's whole claim table on each next-hop change and showed revivals and false ENDs in the sweeps below.
20. ✅ Asked when an origin advances its seqno unprompted: **only when a route link changes**, implemented as a bump when a route link closes. Rejected: only to answer a ROUTE_REQUEST; keeping the per-stream bump.
21. ✅ Asked how one side's routing messages travel so none overtakes another: **one unidirectional Announce Stream per side**, carrying its requests, its answers, and its echoes in order ([the route stream](/quest/m2/cluster-routes/route-stream.md)). It replaces "There MAY be multiple Announce Streams". Rejected: one bidirectional Announce Stream per direction with the echo moved to the answering half; one bidirectional stream per session.
22. ✅ **A side's interest is a set of prefixes edited in place.** The live set never nests, and one edit may swap prefixes for a wider or narrower one without re-sending what both cover ([the interest set](/quest/m2/cluster-routes/interest-set.md)). A token-scope change is such an edit, with one prefix per region of the scope. This supersedes items 16 and 17, and the handoff contract's "An interest change opens a new stream" below, once the route stream lands.
23. ✅ Asked what a floor covers with several prefixes on one stream: **one prefix, named by its id.** This amends item 10: the floor is still a marker and never a region list. Rejected: a floor over the whole set.
24. ✅ Asked how a side learns a newly added prefix's initial set is complete: **SYNCED per prefix, naming its id**, plus one marker per edit where it takes effect for both sides. Rejected: one SYNCED per edit, counted in order.
25. ✅ Asked how a relay safely forgets a publisher that is gone, after the model showed a per-neighbour echo is not enough once the origin sits behind a relay: **each route names the nodes it crossed, and a node never uses a route that names itself** ([moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487), which holds the counterexamples and the model results). Feasibility still selects routes; the list is one per origin route, not the per-claim Hop ID list item 6 removed, and the echo and the drain rule stay as they are. As built: Babel's feasibility condition is unchanged, and a node never selects or certifies through such a route. Where the condition refuses it, it still counts as infeasible, so a relay whose neighbours all route through it holds its claims while one of them finds another way. Under a qualified key it counts only while its hops extend those of the route the node last sent under that key: a copy that a scoped peer hands back under a key derived from the node's own route never moves, so nothing waits on it. The origin names itself too, so a rotated identity's route is refused where it began, and a node names itself by a Hop ID that a rotation leaves alone. The hop limit is the wire's 32 and no lower. A change of hops or RTT alone lets go of no region, unless it changes whether the node may use the route: by crossing it, or by reaching the hop limit. This supersedes the claim, in the handoff contract and in decision 6 below, that the echo alone makes forgetting safe. First decided as a relay echoing a ROUTE_END only once its own downstream had; the model ran that the same day and it fails when a session closes before answering. Rejected: that deferred echo; never dropping the distance; a timer; relays as the only origin nodes.


26. ✅ Decided 2026-11-11 (maintainer, /quest-plan after [moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487)'s reviews), asked whose copy of a publisher's claims a relay believes: **a route is shared per publisher and per scope, and a relay believes a route by that route's own path** ([the prototype](https://github.com/moq-dev/moq/pull/8533), the model (`rs/moq-net/verify/route/RouteHandoff.tla`), then [the router](/quest/m2/cluster-routes/scope.md)). A scope is the prefix a relay asked a neighbour for when its grant there is narrower than its interest. Today only a feasible neighbour counts, one strictly cheaper than the distance the relay remembers, so a relay's claims move whenever a cost or seqno does: it can forget a claim for good, a mesh with two grants never settles after one seqno advance, and behind a scoped neighbour it never shows what a farther one knows. Only a broadcast some relay carries gets its own route, and that route propagates ([cache-aware routing](/quest/m3/cache-routing/README.md) Plan item 8). What keeps a route free of loops and stale copies is item 27. The contract's bullet on which neighbours certify now states the rule the model settled. First decided the same day as any neighbour whose path to the publisher does not pass through the relay; tried on the router, two relays behind a scoped relay then keep a dead claim alive through each other, and the relay behind a scoped neighbour shows a claim it has no route to request. Rejected: that rule; believing a neighbour that is cheaper than the relay is now, which fixed the storm and the forgotten claim with no wire change but rests belief on distance; a path per broadcast as lite-06 has; link-state among relays; routes to relays.

27. ✅ Decided 2026-11-11 (maintainer, on [moq-dev/moq#8533](https://github.com/moq-dev/moq/pull/8533), which measured it), asked what keeps a route free of loops and stale copies: **the route's path, a withdrawal that names the hop that died, holds, and a timed doubt. Babel's distance, the seqno, ROUTE_REQUEST, and the ROUTE_END echo go.** None of it is built: the model (`rs/moq-net/verify/route/RouteHandoff.tla`) checks it, with dead hops kept until their timer fires, [the router](/quest/m2/cluster-routes/scope.md) builds it, [the switch](/quest/m2/cluster-routes/switch.md) puts it on the wire (item 28), and the contract below describes today's router until they do. The rule, as the prototype in `rs/moq-net/src/route/tests/paths.rs` runs it:
    - A hop names a relay and the session it learned the route on. A relay uses the cheapest route whose path does not name it, at once.
    - A relay that loses a session names that hop, and every relay drops every route through it, whoever offered it. One message per session covers every publisher and lists the routes that moved to another path.
    - A relay with no route left holds its claims, unavailable, until each neighbour that learned the route from it has answered: with another way, or with nothing once those behind it have answered. Then it lets go.
    - A closed session casts doubt on the node at its far end, since a dead relay is reported one neighbour at a time. No relay moves to a route through that node until a timer lifts the doubt. The route a relay already uses is exempt.

    Measured on 24 relays (the mesh sweep's sparse shape, 16 seeds), against today's router, with the doubt lasting 4 hops of delay: a detour settles in 12 hops of delay where the distance takes 15.7, and in 6.2 where it does not run through the far end of the closed link; the publisher's relay dying costs 124 route messages where the distance costs 276, with no loop and 6 claims sent again; and a link closing in a clique costs 44 messages with one publisher behind it and 198 with eight, where the distance costs 621 and 5,024. Accepted: a detour through the far end of a closed link waits the timer out; selected routes can loop for a moment when two unrelated things fail at once (32 of 120,000 churn steps), which the distance never does; and holds and doubt have passed sweeps. The model covers both, but its no-stale-route property holds only for one failure and a patient timer; `StaleIsChased` bounds the rest. The timer's length is the model's and the router's to settle, from the maintainer's 1 s. Rejected: Babel's distance; paths with nothing else, which hunt through stale paths (65,219 messages for one publisher leaving a clique of 12); no doubt, which loops when a relay with several neighbours dies; and a doubt lifted by an epoch the node floods, which resolves in a couple of hops but costs a message per session on each relay-to-relay close ("Epoches are a bit scary in terms of messages").

28. ✅ Decided 2026-11-12 (maintainer, /quest-plan on the quests still written for Babel), asked how item 27's removal is split and ordered: **the router first with no wire change, then a switch of the sessions, and the switch lands before the route stream.** [The router](/quest/m2/cluster-routes/scope.md) runs the path rule with a route per scope, sans-IO, beside today's router. [The switch](/quest/m2/cluster-routes/switch.md) puts it on the lite-07-wip wire, the draft, the Rust sessions, and `js/net`, and deletes Babel's router. [The interest set](/quest/m2/cluster-routes/interest-set.md) then edits the path router only, and [the route stream](/quest/m2/cluster-routes/route-stream.md) follows the switch with no echo or route request to carry. The switch's recommended wire puts the answer that ends a hold on the publisher half, behind the routes its sender sent first, so the path rule needs no order between streams that today's sessions lack for one prefix. [Acceptance](/quest/m2/cluster-routes/acceptance.md) is of the switched sessions, and the inductive invariant stays in m1, rewritten for the path, without `LoopFree` as an invariant. Rejected: the router quest in m1; one quest for router, wire, draft, sessions, and `js/net`; the route stream first, which builds the echo and ROUTE_REQUEST on the new stream and then deletes them; one wire change for the path rule and the one stream together; moving the inductive invariant to m4; deleting it. Reversed 2026-11-13 (maintainer, /quest-complete): ✅ the inductive invariant is deleted, since Apalache cannot step even one transition of the route model ([moq-dev/moq#8564](https://github.com/moq-dev/moq/pull/8564)), no Rust TLA+ checker qualifies ([moq-dev/moq#8600](https://github.com/moq-dev/moq/pull/8600), [moq-dev/moq#8669](https://github.com/moq-dev/moq/pull/8669)), and no more JVM model checking is wanted here (rejected: keeping it paused).

The route layer's core landed in
[moq-dev/moq#7676](https://github.com/moq-dev/moq/pull/7676): a
crate-private, sans-IO `Router` in `rs/moq-net/src/route/`, generic over the
neighbor key and the claim attributes, with every holding rule of the
contract below. Each message recomputes only the identity it names. The
handoff fixtures, both sweeps, and the route-layer fixtures of the contract
run against it in `route/tests/`; the forwarding and first-hop selection
fixtures still run only on the reference model in
`rs/moq-net/tests/integration/routing_contracts/`. Nothing drives the router
on the wire yet. The children below carry the rest, in order, and land in
one release: lite-07 carries the route layer as a whole
([lite-07 finalize](/quest/m2/lite07-finalize.md) waits on this epic). This
README holds the routing contract every child implements.

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7676](https://github.com/moq-dev/moq/pull/7676)): land the core
and split the quest into this epic. Options were: ✅ land the core and split;
one PR; the core plus the Rust wire there, with JS as a child.

Settled by the core, for review:

- Every lite-07 claim covers its own path and everything below it, so the
  model's exact-versus-wildcard distinction is gone: a claim above an
  interest overlaps it (`a_claim_above_an_interest_overlaps_it`).
- Letting go of a region keeps the proof's rule: keep every still-certified
  region, or rebuild them all if a lost floor is newer than a kept one, a
  collection whose replay is bounded like any other. The core and the proof
  keep per-region floors internally; the wire carries only item 10's
  whole-interest marker, which session integration ([moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023))
  projects the view onto.
- A rotated identity ends the old one with ROUTE_END on every stream rather
  than closing sessions, since the own identity is no longer proven per
  session.

Keep the 2026-10-01 two-layer direction, reconciled with the maintainer's
2026-10-13 [decisions](/quest/m2/cluster-routing/README.md) and the routing
contract below. The routing contract's remaining model fixtures live in
`rs/moq-net/tests/integration/routing_contracts/` and run against a small reference model;
port each topology to the implementation as acceptance, then delete the model.
The handoff contract below, decided 2026-10-16, settles claim freshness:

The lite-07 wire is specified in `drafts/draft-lcurley-moq-lite.md` (Announce,
Floors, and Routing), and `rs/moq-net/src/lite/announce.rs` encodes and decodes
its new messages ([moq-dev/moq#7764](https://github.com/moq-dev/moq/pull/7764));
the bullets below are the contract it carries.

- The announce cost is the origin's per-prefix seed (`transcode/**` at 10 and
  `transcode/foobar` at 1 from one node, decided 2026-10-01) and nobody
  changes it in transit; a path's cost through a route is that seed plus the
  route metric. Link changes therefore touch only ROUTE, and feasibility runs
  on the route metric alone. This is today's single route cost, which stops
  accumulating per hop once this lands.
- Decided 2026-10-13: the metric is configured additive cost with strictly
  positive link increments, defaulting to one per link. Refuse zero-cost links;
  origin prefix seeds may still be zero. Overflow is unreachable, never
  saturated, using checked arithmetic. No hop-count component is needed
  because each link strictly increases the metric, as Babel's feasibility
  requires (RFC 8966 s3.5.2); a zero-cost or saturated link would leave a
  route equal to its own feasibility distance (`zero_cost_links_are_refused`,
  `overflow_is_unreachable`). Update `?cost=0` examples with
  the implementation and report the breaking configuration change.
- Decided 2026-10-13: GOAWAY gives the peer lowest preference, below every
  eligible non-draining peer, and triggers failover. Retain it as a last resort
  while connected; do not encode drain by saturating the metric. Feasibility
  still gates alternatives, so an infeasible alternative waits for a sequence
  refresh while the draining peer keeps serving
  (`goaway_fails_over_and_keeps_the_draining_peer_last`). Decided 2026-10-14:
  the refresh reaches the node through both peers, and if the draining peer's
  arrives first the node selects it at the new seqno, so it stays on the
  still-serving draining peer until that session closes and then waits for one
  more refresh, which the accepted recovery pause covers
  (`goaway_keeps_serving_until_a_refresh_makes_the_alternative_feasible` pins
  both orders). Using a draining peer's routes locally without advertising
  them was rejected: downstream nodes would lose their only route during the
  drain. Existing readers resume only
  across a matching explicit epoch; epochless changes preserve Restart.
- Fresh routing identity per process/tab restart, declared by the origin and
  shared across its live sessions, decided 2026-10-13. It is unverified.
  Decided 2026-11-02 (maintainer, /quest-unblock): peer grants are dropped and no key proof is
  planned; a provable identity, if a use case needs one, makes this route
  identity a public key, independent of P2P. Decided 2026-11-04 (maintainer,
  /quest-unblock): scoped forgery is that use case; a peer proves a route key
  in its lite-07 SETUP (the Route Key parameter, bound to the session by its
  exporter), and every identity it sends while scoped, its own included, is
  qualified under it. The own identity stays a fresh random id per process:
  peers cannot check one derived from the key and a private salt, so deriving
  it adds nothing.
  Decided 2026-10-26 (maintainer): receivers do not stamp a fresh identity
  per session. In a mesh, two relays stamping their sessions from one origin
  forward its routes as two identities a third relay cannot recognize as
  one, and a declared identity kept beside the stamp as a loop hint would
  stay unverified.
  Switching between paths resumes only across a matching explicit epoch.
  Keep administrative identity and broadcast epoch separate. The origin
  advances its seqno unprompted when a route link closes: a session whose
  peer forwarded it a route, or any session carrying the route of a node
  that forwards nothing. Feasibility distances held elsewhere otherwise
  starve the old neighbors of an origin that moved to a farther attachment
  until a ROUTE_REQUEST reaches it
  (`reconnect_without_restart_needs_a_seqno_advance`). A new link lengthens
  no path, and a client session that forwards nothing changes none. A random identity alone
  does not keep a dead incarnation away. What bounds it is ordered sessions,
  state scoped to the session that delivered it, never advertising an
  unreachable route, keeping the feasibility distance until every
  ROUTE_END it sent is echoed back, and the hops each route carries, which
  refuse a copy of a route this node sent once the distance is gone: a
  collected incarnation then reappears only from a message already in flight
  on a live session that never crossed this node, which delivers the
  retraction next
  (`collected_incarnation_returns_only_while_its_retraction_is_in_flight`).
  `cluster.id` stops naming a routing identity.
- Decided 2026-10-14: every identity a scoped connection sends is
  qualified: filed under a key the receiver derives, rather than its bare
  id, when the connection's publish grant is scoped, and bare when it is
  unscoped (publish `**` at the root). Its own identity is no exception
  (2026-10-26): the bare exemption relied on the dropped proof, so an
  unproven own identity could otherwise claim a victim's. Extend the fixture
  below to a forger declaring the victim's id as its own, and drop the
  reference model's `proved` exemption. The key has its own hash domain, so
  it cannot collide with a key-derived identity, and is collision-resistant
  at the identity width, since the forwarder picks `id` and could otherwise
  grind another peer's key. The relay that files it translates it back when
  forwarding a request into that connection. Claims under it are refused
  outside the grant's scope. Routes are shared by all of an identity's
  claims, so otherwise a scoped peer forging an identity, or declaring a
  victim's id as its own, captures claims outside its namespace and leaves a
  seqno the real origin cannot pass after the forger leaves
  (`forged_identity_hijacks_claims_outside_the_forgers_scope`); with it, a
  forger or a misconfigured uplink only affects its own scope. The `peer`
  claim no longer decides transit trust. Legitimate scoped delivery survives
  losing one of two uplinks under one key
  (`scoped_delivery_through_two_uplinks_reaches_one_key`).
- Decided 2026-11-04 (maintainer): a scoped peer's routing identities are
  keyed by the route key the peer proves, so relays configured with the same
  key file fail over as one; a scoped peer that proves no key, such as one on
  qmux, gets a per-session key
  ([moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)).
  The router takes the key from the caller (`Scope::qualify`).
- Decided 2026-10-14: qualified keys carry a marker every relay recognizes,
  and a scoped connection forwarding an already qualified identity has that
  route refused, with no wire change. Otherwise a key sent down through a
  misconfigured uplink comes back and is filed again as a new identity on
  every lap (`scoped_connections_cannot_requalify_a_qualified_identity`).
  Nested scoped federation is therefore out: the inner level must be
  unscoped. Revisit with a wire change if a use case appears. Rejected:
  keying a forwarded identity by its bare id and the narrowest scope it
  crossed, which needs the scope in ROUTE, and accepting growth bounded only
  by metric overflow.
- Loop freedom is Babel's feasibility condition (RFC 8966) keyed by node:
  accept a route if its seqno is newer, or equal with a metric below the
  feasibility distance. Only the origin advances its seqno. Retraction is
  ROUTE_HOLD or ROUTE_END. The route model found a count to infinity
  (2026-11-10): an echo can overtake a route derived from the echoed one,
  and even in order a per-neighbour echo misses a derived copy held by a
  third relay. Each route therefore names the nodes it crossed, and a node
  never selects one that names itself (Plan item 25,
  `sweep_no_route_outgrows_a_loop_free_path`).
- Starvation recovery is Babel's seqno request (RFC 8966 s3.8, decided
  2026-10-12). A node whose routes to an origin are all infeasible sends
  ROUTE_REQUEST for that origin, with its feasibility seqno plus one, to the
  neighbour it would use. A node holding a route at least that new answers
  with it, and only the origin advances its seqno to answer. Otherwise it
  forwards the request to the neighbour it would use for its own: a
  feasible route's next hop, else an infeasible route's, even when that
  neighbour asked it. This departs from RFC 8966 s3.8.1.2, which avoids the
  requester: nothing cancels a request, so a requester that stopped waiting
  stays excluded, and a hop whose only route runs through it strands the
  request (`sweep_requests_reach_a_quiet_origin`). Next hops form no loop,
  and one pending request per hop stops a bounce. Every node
  holding a request, its own or forwarded, keeps one per origin pending,
  suppressing duplicates, until it selects a route at least that new or has
  no usable route left, and sends it again whenever the neighbour it would
  send it to changes. No retry timer and no hop count: a looping copy meets
  its own pending entry and stops. Decided 2026-10-16 from the handoff
  proof: the earlier sketch (a few timed resends, then wait for the next
  infeasible update) leaves a node starved for good behind a quiet origin
  when every attempt is dropped by a forwarder whose retraction is still in
  flight, and the previous hop then moves to an equal-metric route that
  changes nothing it advertises (`a_quiet_origin_stays_starved_after_bounded_retries`).
  Recovery is eventual without timers: a drop happens only at a node with no
  usable route, whose retraction then changes the previous hop's next hop
  and resends the request, and once the topology is quiet the selected next
  hops, loop-free under feasibility, carry it to the origin. Feasibility is
  never reset to accept a longer route after a retraction or a session replay.
- Feasibility makes a detour longer than the lost route wait for that request
  to reach the origin, where today's path vector fails over at once. Redundant
  cores are feasible successors and switch immediately
  (`redundant_cores_fail_over_without_a_seqno_request`); a local mesh reaching
  its other half through the CDN, or a routing node falling back from P2P to
  the CDN, starves until the origin answers
  (`partitioned_local_mesh_reaches_itself_through_the_cdn`,
  `p2p_fallback_is_immediate_for_leaves_and_needs_a_seqno_for_transit`).
  Decided 2026-10-13: accept this recovery pause; instant failover is not a
  requirement. Measure transport failure detection, additional sequence-refresh
  recovery, and total interruption separately. The fixtures' manual origin
  bump proves feasibility recovery; the handoff fixtures also run the request
  exchange, though not its latency.
- Claim freshness follows the handoff contract below, independently of the
  requested path's forwarding choice.
- Decided 2026-10-13: preserve application priority among feasible transit
  routes, then compare additive cost. Feasibility always takes precedence:
  a preferred longer route can remain ineligible after a shorter route was
  advertised, so session arrival order can affect the eventual selection
  (`transit_priority_depends_on_arrival_order`). This limitation is accepted;
  priority remains local policy, not a propagated metric tier. Endpoints
  that advertise no transit routes hold no transit feasibility distance.
- Decided 2026-10-14: one selection per identity, keeping the `upstream`
  link mark and its down-only bit. A node advertises its best route whether
  or not anything subscribes. A route learned on an upstream link, or through
  a neighbor's down-only route, is down-only, and a down-only route is never
  sent to an upstream link, so CDN relays never shortcut between each other
  through a local network. CDN content still flows into the network and local
  content out, including through a peer whose own uplink died. The relay that
  accepted a request from an upstream link ends that forwarded subscription
  when its selected route turns down-only, and the first hop re-resolves; no
  request bit goes on the wire. One selection hides a cheaper exportable
  route whenever the preferred route is down-only
  (`preferred_down_only_route_hides_the_exportable_alternative`); this is
  accepted, and ranking local peers above a backup uplink avoids it. The
  two-view model it replaces lost its export restriction one LAN hop after an
  upstream ingress, and identity-only aggregation deadlocked across views
  (`cdn_never_reaches_its_own_relay_through_the_lan`,
  `upstream_ingress_ends_a_request_whose_route_turns_down_only`,
  `dead_uplink_consumes_and_publishes_through_a_lan_peer`). A
  misconfigured uplink that leaks CDN routes back is contained by
  qualified identities rather than a downstream mark
  (`misconfigured_uplink_is_contained_by_qualified_identities`).
- Decided 2026-10-14: a local per-connection `proxy` preference decides
  whether a connection is sent routes learned from other connections; it is
  never on the wire. Relays default on; applications choose per P2P
  connection; browser tabs stay off until the Rust-to-TypeScript port.
- A plain client (`proxy` off) advertises its own origin and publications only, under
  the one identity it declares on every live session. Clients receive only authorized, interested claims; opaque ids expose no
  backbone addresses, but are not themselves authorization. A client
  advertises nothing it receives, so it holds no feasibility distance and
  falls back from P2P to the CDN at once. It still echoes every ROUTE_END.

Handoff contract, proven 2026-10-15 by the fixtures in `handoff.rs` against
next-hop authority and a snapshot barrier alone. A next-hop change adopts the
newest knowledge any session has, never an older complete snapshot, and route
loss never reports an END:

- Each origin numbers the changes to its own claims with a revision that
  starts at zero for each routing identity, separate from the route seqno.
  Both are varints. An origin never advertises the last value, 2^64-1 (the
  largest lite-07 varint): a
  seqno advance, claim change, or seqno request that would reach it rotates
  the identity instead, ending the old one everywhere and announcing the
  claims again under a fresh one, so nothing wraps and every advertised
  seqno leaves room for the successor a request names
  (`exhausted_counters_rotate_the_identity`).
  A session sends an origin's announcements in batches, each closed by its
  floor: the revision at which the stream's announcements for that route,
  across the whole interest, are the origin's, or nothing when the sender
  does not know them across the interest at one revision (Plan item 10). A
  qualified key is known only within the grant scopes it arrived through,
  and one key can arrive through grants whose pattern unions differ, so the
  view inside a node carries one floor per scope, and a stream whose
  interest is wider than those scopes certifies nothing.
- A receiver keeps each session's last view, to apply the next batch, and
  merges every session's view into its own claim by claim: whichever view
  covers the claim with the newer floor decides. A floor certifies only the
  claims overlapping its prefix: those inside it and wildcards above it, not
  an exact claim above it
  (`only_a_wildcard_claim_overlaps_an_interest_below_it`). A node
  keeps floors only within its current interest, dropping any that a wider,
  newer floor covers. An older complete snapshot is therefore ignored, where
  next-hop authority and a barrier both revive the withdrawal
  (`an_older_complete_snapshot_revives_a_known_withdrawal`); a new next hop
  whose ROUTE precedes its announcements changes nothing the reader sees
  (`next_hop_authority_reports_false_absence_while_announcements_trail`);
  widening keeps a narrower, newer floor yet still accepts an older, wider
  snapshot for what it newly covers, where one floor per origin either
  refuses that snapshot or revives under it
  (`widening_interest_needs_floors_scoped_to_what_they_cover`). An interest
  change opens a new stream: batches still in flight on the old one are
  discarded, and each neighbour starts the new one with a complete
  snapshot. The first batch answering the new interest retires whatever
  lies outside it, so state follows the current interest, not every prefix
  ever watched; a rejoin then replays a lagging relay's view, as after a
  collection, only until the newer snapshot behind it lands
  (`cancelling_interest_retires_its_region`). That batch re-certifies any
  wildcard above the interest, so retiring never ends a claim still
  watched. Until it arrives, a widened node hides what it has not retired
  yet, rather than showing a claim that ended unwatched
  (`widening_hides_what_was_frozen_until_a_covering_floor_arrives`); a root
  snapshot still queued on the old stream does not count
  (`a_snapshot_queued_before_widening_keeps_the_frozen_rest_hidden`), and
  a batch cut to a scoped grant does
  (`widening_behind_a_scoped_grant_shows_the_rest_of_the_scope`). A batch
  certifies the interest only within its session's grant scope, so a view
  merged from several grants stays hidden in each scope until that grant
  answers (`widening_behind_two_grants_waits_for_each_scope`). A floor
  outside the interest stays while it is newer than every floor inside,
  since it may certify a wildcard above both at a later revision; and
  withdrawing one origin's wildcard leaves another origin's copy
  (`wildcard_claims_end_per_origin_and_are_certified_below`).
- Not yet built ([the router](/quest/m2/cluster-routes/scope.md)
  builds it and [the switch](/quest/m2/cluster-routes/switch.md) puts
  it on the wire; the rest of this contract describes today's router until
  then),
  settled by `rs/moq-net/verify/route/RouteHandoff.tla` with Plan item 27's
  rule: a route is keyed by its publisher and the prefix it covers, and
  carries its own path. A node that asks a neighbour for a narrower prefix
  gets one route there, covering that prefix: the neighbour's live route for
  that prefix if it has one, else its route for a prefix above it. A node
  believes a route whose path does not name it, and keeps one view per
  prefix: the newest floor any route covering that prefix gave it, kept
  whole while it holds a route covering the prefix, so a lagging route
  cannot revive a withdrawal another delivered, and a route that ends takes
  only what no other route covers. A claim shows from the narrowest prefix's
  view and is available while a live route covers it. A request starts on
  the narrowest live route covering its claim, follows the route each node
  offered the one it came from, and is never forwarded along a route whose
  path names the node it came from. A node remembers a dead hop while a
  neighbour's route names it and until the timer the hop's close started
  fires, so no route through a hop it knows is dead returns. Selected routes
  can loop only while the news that ends a stale route is on its way, and
  only through at least one stale route. What its timer costs fired early, and the loop `Loop.cfg` pins, are in the `verify-route` skill.
  A sender sends a route on a stream only while the route meets the
  stream's interest (Plan item 15), and a node presents each stream only the
  claims that changed.
- Per identity, a node is usable (a feasible selected route: announcements
  shown available), held, or let go. It is held while starved, or while a
  neighbor's held route has a last advertisement feasible here; it
  keeps showing the announcements as unavailable and sends ROUTE_HOLD. Holds therefore follow strictly decreasing metrics
  down to a starved node and cannot prop each other up. Otherwise the node
  lets go: ROUTE_END to every neighbor it advertised to, and its reader sees
  the announcements go offline, a distinct event from an END the origin
  sent; while held, the reader sees them unavailable. The
  detour window keeps presence at the starved relay and behind it
  (`a_starved_relay_keeps_presence_unavailable_until_its_request_recovers_it`);
  a dead origin or a total partition goes offline everywhere with no timer
  (`holds_retire_once_nothing_can_recover`). No promise is made of learning a
  withdrawal during a partition.
- A node may collect an identity's views and floors once it holds no route
  entry for it; correctness does not depend on when. It drops the
  feasibility distance once it also advertises the identity nowhere and
  every neighbour has echoed each ROUTE_END it sent, or closed. Until then
  an advertisement derived from its own can still wait on some session, and
  accepting it loops; once all echoes are back no neighbour routes through
  it (`collection_keeps_the_feasibility_distance_against_a_queued_echo`),
  and a copy a third node still holds names this node in its hops, so it is
  refused when it returns
  (`a_route_handed_round_a_triangle_is_refused_where_it_began`). The
  echo is per neighbour, not a diffusing computation: every other node
  keeps its own distance. A stale route accepted after the drain dead-ends at
  a node that let go, like any collected incarnation.
  After collection, or a process restart, a stale view returns only from a
  sender that has not yet received the withdrawal, and only until it does
  (`stale_replay_after_collection_lasts_only_until_the_withdrawal_behind_it`).
  Retained state is each session's last view, one merged view per held
  identity with floors only within the current interest (and any newer one
  certifying a wildcard above it), one pending
  request per origin, and a feasibility distance only while an identity is
  routed, advertised, or draining; neither withdrawal history nor interest
  history leaves anything. One claim change costs one
  batch per session carrying the route and one event per watching node
  (`announcement_cost_follows_sessions_and_live_claims`). Without split
  horizon a relay echoes an origin's claims back to it.
- A fresh identity per restart also starts a fresh revision space; reusing
  an identity would need its revision persisted along with its seqno
  (`origin_restart_takes_a_fresh_identity_for_its_claims`).
- Presence and availability are separate. A forwarded subscription follows
  only a usable route, so a held route ends it as unreachability does while
  the reader's announcement stays, unavailable. When the selected route
  moves between usable next hops, keep the same-epoch subscription on the old
  one until the new one delivers; epochless changes remain Restart. The model
  does not cover the data path; the route-change acceptance tests do.
- `sweep_handoff` churns 150 random meshes per rule: two origins and a
  client, claims changing, sessions dropping and returning, messages held
  and delivered out of step, interest changing, origins restarting with
  fresh identities, and route-less identities collected. Merged floors keep
  every view equal to some revision of the origin's truth after every step,
  and once churn stops every node shows each live origin's current claims and
  holds nothing for a dead one. `sweep_scoped_handoff` churns a client
  behind two scoped grants across interests inside, above, and outside both
  scopes, with no false END. Over `sweep_handoff`'s scenarios:

  | Rule | Revivals | False ENDs | Re-announcements | Reader events | Claim KiB |
  | --- | --- | --- | --- | --- | --- |
  | Next-hop authority | 386 | 424 | 393 | 8853 | 856 |
  | Snapshot barrier | 231 | 175 | 260 | 9086 | 825 |
  | Merged floors | 0 | 0 | 30 | 9020 | 799 |

  Merged floors' re-announcements follow an offline period or a collection,
  never a known withdrawal. Byte counts are estimates in virtual time, and
  charge each batch its full region list,
  with paths as suffixes of the interest.

Decided with the maintainer 2026-10-16, from the handoff proof:

1. Floor encoding: ✅ one ANNOUNCE_FLOOR closes each batch, and ANNOUNCEs
   carry no revision. Its regions came with decisions 7 and 8. Rejected: a revision on every ANNOUNCE,
   which adds bytes and removes no state, since a receiver still needs each
   session's last view to apply a batch from a sender whose own view jumped;
   and the floor on ROUTE_UPDATE, which ties claim churn to route processing.
2. Held encoding: ~~an infinite metric (2^64-1) means held and ROUTE_END
   means let go, with no new field. Rejected: an explicit held bit.~~
   Superseded 2026-11-05 by Plan item 14: ROUTE_HOLD means held, and
   ROUTE_END still means let go.
3. Reader API: ✅ an announcement carries availability, and letting go is a
   distinct offline event, separate from the origin's END, in Rust and JS.
   Rejected: presence alone, which gives a reader nothing to re-request on
   when a held route recovers; let-go reported as END, as today; and
   availability with let-go kept as END, which hides whether the origin
   ended or the network lost it.
4. Collection: ✅ collect routes and announcements once no neighbor's route
   entry remains, held entries included. `sweep_handoff` checks selected
   routes stay loop-free at every step; collecting while a neighbor's entry
   remains loops at once. Rejected: Babel's source GC timer.
   Revised after review: an empty route table does not prove that an echo of
   the node's own advertisement has drained from every session, and
   accepting that echo loops
   (`collection_keeps_the_feasibility_distance_against_a_queued_echo`), so
   decision 6 drains the feasibility distance instead.
5. Seqno requests: ✅ one pending request per origin at every hop, resent on
   a next-hop change, with no timer and no hop count. Rejected: the
   bounded-retry sketch, which strands a quiet origin's node.
   Revised 2026-10-16 after review: a hop forwards to its next hop even when
   that neighbour asked it, since excluding requesters strands requests
   (`sweep_requests_reach_a_quiet_origin`).
6. Feasibility-distance retention: ✅ drain it. A receiver echoes every
   ROUTE_END back on the same stream once applied, and a node drops the
   distance once it routes and advertises the identity nowhere and every
   echo is back, a closed session counting as one. Reusing ROUTE_END adds no
   message type. The neighbour's own ROUTE_END cannot be the reply: a client
   or a node whose copy is infeasible never starts a route back, so the
   distance would leak for every identity sent to a client. A per-neighbour
   echo is enough, with no diffusing computation: once none holds the
   advertisement, no node selects through this one, and every other node
   keeps its own distance. A restart is the same drain, since its sessions
   close with the process
   (`restart_drops_the_feasibility_distance_with_every_session`).
   `sweep_handoff` interleaves echoes freely with routes, stays loop-free at
   every step, and leaves no distance for a dead identity; dropping the
   distance before the echoes return fails the queued echo fixture, and
   dropping it at no entry loops where the echoed condition never does
   (`sweep_forgetting_distances`). Revised 2026-11-10 (Plan item 25): the
   echo is not enough alone, since a third node can hold a copy derived from
   the advertisement and hand it back after the drain. Each route names the
   nodes it crossed, and a node never selects one that names itself
   (`sweep_no_route_outgrows_a_loop_free_path`). Rejected: keeping the distance forever,
   which grows with publisher churn times uptime on every relay that saw an
   identity, about 16 bytes each, until a restart; and Babel's source GC
   timer, which assumes stale echoes are gone by then rather than proving it.
7. Scope coverage under one key: ✅ per-region floors (superseded on the
   wire 2026-11-05 by Plan item 10; the core keeps regions internally). ANNOUNCE_FLOOR
   names the region it certifies, a suffix of the stream's interest prefix
   (the scope met with the interest), each node tracks which regions each
   feasible neighbour's last view certifies, and a region none certifies
   goes offline. One key can arrive through grants whose pattern unions
   differ, and without this a scope whose last session closed stays shown,
   available, with no way to learn its withdrawal; a relay forwarding such a
   key also needs one floor per scope on a stream. Rejected: one pattern
   union per key, which refuses or re-files a refresh that changes the
   union and cannot be checked across relays; and deferring to the
   implementation, which leaves the proof's freshness claim false for scoped
   delivery.

8. Region-set framing (superseded 2026-11-05 by Plan item 10, a
   whole-interest marker): ✅ one ANNOUNCE_FLOOR per batch lists every region
   the sender certifies for the route: (Route ID, count, then a region suffix
   and revision per region). The list replaces the last, so a sender stops
   certifying a region it lost by leaving it out, and a count of zero answers
   an interest it cannot certify, which its peer needs to stop waiting
   (`a_disjoint_interest_is_answered_with_nothing`). The common case is one
   empty suffix, one byte over (Route ID, revision). Rejected: one region per
   ANNOUNCE_FLOOR with revision 0 dropping a region, which gives revision 0 a
   meaning and costs a floor per dropped region; and an end-of-batch marker,
   a message for what one floor frames.

Why not a hold-down: [moq-dev/moq#4644](https://github.com/moq-dev/moq/pull/4644) retracted at once and re-announced a replacement
after 1 s, which ended a downstream front whose only path ran through the
relay, dropping subscribers for about a second while the relay kept serving;
holding without retracting brought hunting back in measurements, and
#4642's 300 ms cursor hold was the earlier mitigation. Main also contains
[moq-dev/moq#5375](https://github.com/moq-dev/moq/pull/5375)'s two-second retraction quarantine and
[moq-dev/moq#5391](https://github.com/moq-dev/moq/pull/5391)'s same-clock queue dedup.
#5375 measured residual stale revivals, so neither longer holds nor one passing
mesh establish correctness. Separating reachability from claim freshness is
the direction, not yet a proof. Retire mitigations only after the incident
fixtures and failover tests demonstrate their replacement.

Simulator findings (2026-10-12), measured with seed 1 on a tiered
45-node topology (four regions of two upstream nodes and their edges, plus
five single-node regions), tiered synthetic graphs of 347 and 1024 nodes,
and a six-node radio mesh with two uplinks. Bytes are uncompressed estimates
with eight-byte ids; these are protocol metrics in virtual time, not runtime
benchmarks.

- Starvation: a five-node ring loses its preferred origin link while a
  working four-hop backup remains. The backup's same-seqno route stays
  infeasible for good, and restarting the backup's session does not help;
  only an origin seqno advance does. Hence the seqno request above.
- Ending a broadcast on the 45-node topology costs 124.8 KiB against path
  vector's 2817.0 KiB (1304.0 KiB with #4399's withdrawal hiding), but still
  2000 client updates against a 900 once-only minimum (up to 11 per node
  and path), with 550 re-announcements. The synthetic graphs reach about
  three times the minimum (20640 against 6940, 62182 against 20480).
  Next-hop authority alone does not meet the Goal's "reaches each node
  about once".
- Losing one of an edge's two upstream links leaves live broadcasts reported
  as gone for 722.8 aggregate seconds before restoration, where path vector
  reports none: the route moves, but the announces behind it trail.
- The down-only bit holds: with an uplink edge cut off from both of its
  upstream nodes but still linked to the mesh, path vector routes its
  subscriptions through the radio mesh and the split refuses, and after a
  partition each island reaches the other through its uplink.

## Related

- [Route wakes](/quest/m4/route-wakes.md) - index work follows the settled source-selection model
