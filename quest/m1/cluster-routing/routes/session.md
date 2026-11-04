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

## Waiting

- Maintainer: how a scoped peer's routing identities are keyed. The router
  takes the key from the caller (`Scope::qualify`), so nothing else waits on
  it. Recommendation: (1), a route label carried in the grant.

  What the key is for. Every origin declares a routing identity: an opaque
  number nobody verifies. A peer admitted under a scoped grant (say it may
  publish only `acme/**`) could declare a victim's identity. One route
  serves every claim of an identity, so the forgery would capture the
  victim's claims outside `acme/`, and its seqno would outlive it
  (`forged_identity_hijacks_claims_outside_the_forgers_scope`). So a relay
  files every identity a scoped peer sends under a qualified key, derived
  from something about that peer and the identity, and counts claims under
  the key only inside the grant. The question is what that something is.
  It must stay the same across token refresh, be shared by one operator's
  sessions so losing one uplink fails over instead of restarting readers
  (`scoped_delivery_through_two_uplinks_reaches_one_key`), differ between
  operators, and work for sessions admitted with no token (public rules,
  mTLS certificates, or a third-party auth server).

  Why not the credential subject: `hash(subject, id)` was decided
  2026-10-15 and dropped by the maintainer 2026-11-02, together with
  refusing scoped tokens that carry no subject; the implementation is in
  [moq-dev/moq@453e1ec](https://github.com/moq-dev/moq/commit/453e1ece7c5435ee19541cfc2e9bbe0b6fbc430e).
  It existed only for tokens, so token-less admission had no key. Rejected
  earlier: the grant's pattern union, which refresh and withdrawal change
  and two uplinks need not share, and the connection root, which two scoped
  operators under one root would share.

  Example: operator Acme runs a local relay L with uplinks to CDN relays C1
  and C2. Each admits L with a token scoped to `acme/**`, refreshed hourly.

  1. A route label carried in the grant. Whoever issues it names it: a
     token claim such as `route: "acme"`, a public rule's entry, or an mTLS
     mapping. The key hashes the label with the identity in its own domain.
     Both of Acme's tokens say `acme`, so C1 and C2 file L's origins under
     one key, refresh keeps it, and losing C1 fails over through C2.
     Another operator's label differs. A grant with no label gets a
     per-session key, as in (2). Costs a grant field in `moq-auth`,
     `js/auth`, and the docs, and issuers must not hand two operators one
     label.
  2. A per-session key, with nothing to configure. L's sessions to C1 and
     C2 get two keys, so losing C1 restarts the readers it served onto C2's
     key instead of failing over, and so does a refresh that reconnects.
     Simplest; gives up the two-uplink fixture.
  3. A key the relay operator configures per scoped remote, such as C1 and
     C2 both mapping admissions under `acme/**` to `acme`. No grant change,
     but every CDN relay's configuration must agree, and it names patterns
     rather than who connected, so two operators admitted under one pattern
     share a key.

## Required

- [lite-07 route wire](/quest/m1/cluster-routing/routes/wire.md) - the messages this drives
- [Listing and follow](/quest/m1/api-audit/route/events.md) - the announcement events this feeds ([moq-dev/moq#7678](https://github.com/moq-dev/moq/pull/7678))

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes/README.md) - the contract
- [Route wakes](/quest/m2/route-wakes.md) - index work follows the settled source-selection model
