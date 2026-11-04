# [XL] Route session integration

## Goal

One `Router` per origin node, fed by every lite-07 session, drives that
node's origin and every announce stream it answers. Its shown claims and
selected routes become the origin's routes, its outbox becomes each
stream's ROUTE and ANNOUNCE messages, and the first hop pins each request
to the identity it selected. A lite-06 peer is translated at the relay. A
peer can make the router hold only bounded state. The configuration
changes the contract decided ship with it.

## Plan

- Scope route state to interested origins, not the CDN's whole node table.
  Send a route before any announcement referencing it. The epic's handoff contract
  defines held routes, letting go, and collection.
- ✅ Decided 2026-10-15 (maintainer): retract and re-file when a session's
  grant union crosses the scoped boundary in either direction (a `**` token
  added, or its expiry). Every forwarded route the session delivered is
  retracted, and its current advertisements are re-filed under the new key,
  so no route, feasibility distance, or alias stays under the old one.
  Expiry cannot be refused, so refusing the transition was not an option.
- Only the first hop selects the origin. It keeps `route_order`'s shape over
  the origin nodes announcing the path: longest prefix, newest epoch (within
  one instance only, per [Instance and epoch](https://github.com/moq-dev/moq/pull/7489),
  decided 2026-10-30), the link's priority (`origin::Producer::with_preference`), seed plus route
  metric, then a rendezvous hash of the path and the identity, and the newest
  connection only for an otherwise identical route. The hash drops the hop
  chain, which differs per node, so every node agrees on a pool member
  (`pool_ties_agree_only_on_a_path_and_identity_hash`). The request then names
  that identity, and every later hop follows its own route to it, so
  forwarding needs a route but not the claim.
- Reselecting the origin at every hop loops although each origin's routes are
  loop-free (RFC 8966 s2.7): a ring of same-epoch origins whose relays
  prioritize different links, and nested wildcards where a relay has not
  heard the specific claim, both cycle past requester exclusion; joining
  another relay's subscription by path or instance deadlocks; and without an
  instance a later hop splices another publisher under the requester's
  announcement with no Restart (`local_priorities_loop_path_only_requests`,
  `overlapping_wildcards_loop_where_specific_claims_are_unheard`,
  `aggregation_cycles_starve_without_a_forwarding_constraint`,
  `reselection_without_an_instance_splices_another_publisher`,
  `sweep_forwarding_progress`). A relay therefore joins a forwarded request
  only to a subscription following its route to the same identity. When that
  identity becomes unreachable the forwarded request ends, and the first hop
  re-resolves, resuming only on a matching epoch. A request carrying a cost
  bound instead also never loops, but stalls where a transit hop has not
  heard the claim and still splices epochless sources.
- Mixed versions: a lite-06 peer keeps today's path vector, translated at the
  relay that speaks both, for the rollout window only.
- Interest changes: key the router by announce stream, not session. The old
  stream stays open, its routes counting and its batches discarded, until
  the new stream has answered, so no route drops in between. A stream is
  just another neighbor key to the router.
- A batch carries changes against the last batch on its stream. Restricting
  a view to an interest is still linear in that origin's claims per change;
  make it incremental here.
- Bound what a peer makes the router hold (routes, claims, views per
  session) by design, as the session caps announcements today.
- Public API: the route-change surface on `broadcast::Route` and its
  bindings will likely change, `Hops` and saturating `Cost` go, zero-cost
  links are refused (update `?cost=0` examples and report the breaking
  configuration change), a per-connection `proxy` preference is added, and
  `cluster.id`/`--cluster-id` is removed rather than kept as an unused label
  (decided 2026-10-13). `Preference` continues to rank feasible transit
  routes; report it.
- Open, for the implementer to settle and record: how much of today's route
  trie, fronts, and `route_order` survives intact.

Decided 2026-10-27 (maintainer): the route layer deletes the epochless instance
machinery in `rs/moq-net/src/model/origin.rs`: `Instance::Route(generation)`
and its renewals, `restarted_since`, the held-restart Release for epochless
routes, and Selection retirement, leaving Follow's same-epoch
Update/Restart split as the only takeover rule. Once each origin is its own
ANNOUNCE and the first hop pins the request to its identity, the first hop's
route is the instance, so the per-hop renewal has nothing left to protect.
An epochless change of serving route stays a Restart, including the
same-origin next-hop change in the [wire sketch](/quest/m1/cluster-routing/routes/wire.md), and the routing
contract's epochless fixtures keep passing. Minting an epoch for every route
instead was tried and rejected
([moq-dev/moq#6948](https://github.com/moq-dev/moq/pull/6948)): each relay
mints at its own time, so mint order outranks cost, preference, and the
local-broadcast tie and relays disagree on a pool path's winner, while
ranking a minted epoch last keeps the renewal it was meant to delete.

Moved 2026-10-30 (maintainer, the [route epic](/quest/m1/api-audit/route/README.md)):
[Instance and epoch](https://github.com/moq-dev/moq/pull/7489) now deletes that
epochless machinery, so the route layer no longer does. A session mints
an `instance` (`~` plus random bits) for any route that
arrives without one, and newest-wins compares epochs only within one
instance, so mint order no longer outranks cost or preference. The rest of
the 2026-10-27 decision stands. The broadcast epoch fields here become that
publisher instance and its per-instance epoch, in place of the UUIDv7
epoch. The route layer's own origin-node identity, with its seqno and
revision, stays separate ("keep administrative identity and broadcast epoch
separate", in the epic).

Decided 2026-11-04 (maintainer, /quest-unblock): a scoped peer's identities
are keyed by a route key the peer proves, specified in
[Scoped peers prove a route key](/quest/m1/cluster-routing/routes/proof.md).
A proven peer's own identity is derived from its key and stays bare; every
identity it forwards is qualified under its proven key. A peer that proves no
key gets a per-session key. `Scope::qualify` still takes the key from the
caller, so this quest does not wait on the proof. Options shown:

- ✅ A route key the peer proves once per announce stream, before the lite-07 cut
- A route label carried in the grant (the earlier recommendation), with the
  proven key added after the cut
- A per-session key now
- A key the relay operator configures per scoped remote

Research behind it: proving a neighbor's own key cannot remove qualification,
since a public key is public and a scoped peer could still forward a victim's
key with an inflated seqno; origin-signed routes still leave cost lies and
blackholing, at the price of per-update signatures and expiry refreshes. Keyed
by a proven key, every remaining attack stays inside the forger's grant, with
no grant field, no issuer-coordinated labels, and support for token-less
admission.

## Required

- [lite-07 route wire](/quest/m1/cluster-routing/routes/wire.md) - the messages this drives
- [One Route handle](/quest/m1/api-audit/route/handle.md) - the origin's route surface this rewrites
- [Listing and follow](/quest/m1/api-audit/route/events.md) - the announcement events this feeds ([moq-dev/moq#7678](https://github.com/moq-dev/moq/pull/7678))

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes/README.md) - the contract
- [Route wakes](/quest/m2/route-wakes.md) - index work follows the settled source-selection model
