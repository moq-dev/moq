# [XL] Routes and announces

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

Keep the 2026-10-01 two-layer direction, reconciled with the maintainer's
2026-10-13 [decisions](/quest/m1/cluster-routing/README.md) and the routing
contract below. The routing contract's fixtures live in
`rs/moq-net/tests/routing_contracts/` and run against a small reference model;
port each topology to the implementation as acceptance, then delete the model.
The sketch stays conditional on Next-hop handoff, which settles claim
freshness before coding:

- Wire sketch, flexible in detail. ROUTE_START carries the node id, a seqno,
  the metric, and the down-only bit (below), and implicitly takes the next
  stream-local Route ID, as ANNOUNCE_START takes the next Announce ID today.
  ROUTE_UPDATE (Route ID, seqno, metric, down-only) and ROUTE_END (Route
  ID) reference it; the bit rides UPDATE so a route moving between a
  mesh path and a CDN fallback changes it without retracting its announces.
  ANNOUNCE_START carries the prefix (keeping today's Path Base/Keep
  compression), a Route ID, the origin's cost for that prefix, and the epoch,
  and lite-07's restart message from
  Restart keeps its meaning. On a
  route without an epoch, the source is the origin node and its ANNOUNCE: a
  new one at the same path is a restart. This is routing provenance, not
  permission to stitch. A concrete serving-route change without an epoch
  remains Restart even when the origin node is unchanged; only a matching
  explicit epoch or the separately proven Takeover operation preserves an
  instance. Test a same-origin next-hop change with no epoch.
  ANNOUNCE_UPDATE re-prices it and ANNOUNCE_END withdraws this origin's claim
  while the route stays up. A wildcard claim is capability, not an inventory
  of broadcasts; another origin may still serve the same epoch. An announce
  never changes its Route ID: another origin
  serving the same path is another ANNOUNCE. An ANNOUNCE naming an unknown
  Route ID is a protocol violation. ROUTE_END removes this stream's
  reachability, not proof that the
  origin withdrew its claims. Preserve known presence through uncertain
  handoff with forwarding availability represented separately and bounded
  state, following the handoff proof's reference/retirement contract.
  ROUTE_REQUEST (node id, seqno, hop count) travels
  the other way on the same stream, toward the neighbour that sent the route.
  The hop list leaves lite-07, and with it the
  `Hop Base`/`Hop Keep` compression.

  ```text
  ROUTE_START  node=0x7a3f seqno=41 metric=12   -> route 0
  ANNOUNCE_START prefix=transcode/ route=0 cost=10    -> announce 0
  ANNOUNCE_START prefix=transcode/foobar route=0 cost=1 -> announce 1
  ROUTE_UPDATE route=0 seqno=42 metric=15
  ANNOUNCE_END announce=1                       # this origin withdraws a claim
  ROUTE_END    route=0                          # this stream loses reachability
  ```
- Scope route state to interested origins, not the CDN's whole node table.
  Send a route before any announcement referencing it. The handoff contract
  defines unavailable routes and reference retirement, including partial
  interest, so collection neither invents a publisher END nor permits stale
  claims to revive after rejoin.
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
- Fresh routing identity per process/tab restart, shared across its
  authenticated live sessions, decided 2026-10-13. It is the hash of a
  per-process key that every session proves once
  ([Identity proof](/quest/m1/cluster-routing/identity.md), decided
  2026-10-14). Keep administrative identity and broadcast epoch separate. The shared identity recognizes an
  origin reached through CDN and direct links and maps it to a roster peer.
  The origin advances its seqno whenever its own set of sessions changes:
  feasibility distances held elsewhere otherwise refuse a restart that reuses
  an identity, and starve the old neighbors of an origin that moved to a
  farther attachment (`restart_reusing_identity_is_refused`,
  `reconnect_without_restart_needs_a_seqno_advance`). A random identity alone
  does not keep a dead incarnation away. What bounds it is ordered sessions,
  state scoped to the session that delivered it, never advertising an
  unreachable route, and keeping a feasibility distance while any route to
  that identity remains: a collected incarnation then reappears only from a
  message already in flight on a live session, which delivers the retraction
  next (`collected_incarnation_returns_only_while_its_retraction_is_in_flight`).
  `cluster.id` stops naming a routing identity.
- Decided 2026-10-14: a connection's own proven identity stays bare. An
  identity a connection forwards for another origin is subject-qualified:
  filed under `hash(subject, id)` when the connection's publish grant is
  scoped, and bare when it is unscoped (publish `**` at the root). The hash
  has its own domain, so it cannot collide with a key-derived identity, and is
  collision-resistant at the identity width, since the forwarder picks `id`
  and could otherwise grind another subject's key. The relay that files it
  translates it back when forwarding a request into that connection. Claims
  under it are refused outside the grant's scope. Routes are shared by all of
  an identity's claims, so otherwise a scoped peer forging an identity
  captures claims outside its namespace and leaves a seqno the real origin
  cannot pass after the forger leaves
  (`forged_identity_hijacks_claims_outside_the_forgers_scope`); with it, a
  forger or a misconfigured uplink only affects its own scope. The `peer`
  claim no longer decides transit trust. Legitimate scoped delivery survives
  losing one of two uplinks under one key
  (`scoped_delivery_through_two_uplinks_reaches_one_key`).
- ✅ Decided 2026-10-15 (maintainer): the qualifier is the grant's credential
  subject, and the key is named `hash(subject, id)` (previously
  `hash(scope, id)`); "scope" now means only a grant's pattern union. A
  publish grant is a union of patterns that refresh and withdrawal change, so
  neither the whole union nor one matching pattern is a key that survives
  refresh and that two uplinks share. The subject stays stable across token
  refresh and is shared by one operator's uplinks; each claim is still checked
  against the current pattern union. Rejected: the connection root, too
  coarse since two scoped peers under one root would share keys. Add
  multi-pattern, overlapping-pattern, and refresh fixtures. Today
  `moq_auth::Claims` drops `sub` and `moq_net::auth::Grant` holds only
  patterns and expiry, so the grant gains the verified subject, qualified by
  its issuer, in Rust and JS. Fail loud: a scoped token without a subject is
  refused, and so is a token added to a session (refresh or `auth().add`)
  whose issuer-qualified subject differs from that of the session's current
  grants. The receiver derives the key from the token it verified, so no wire
  change.
- ✅ Decided 2026-10-15 (maintainer): retract and re-file when a session's
  grant union crosses the scoped boundary in either direction (a `**` token
  added, or its expiry). Every forwarded route the session delivered is
  retracted, and its current advertisements are re-filed under the new key,
  so no route, feasibility distance, or alias stays under the old one.
  Expiry cannot be refused, so refusing the transition was not an option.
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
  feasibility distance. Only the origin advances its seqno. Retraction is an
  infinite metric or ROUTE_END; there is no count to infinity.
- Starvation recovery is Babel's seqno request (RFC 8966 s3.8, decided
  2026-10-12). A node whose routes to an origin are all infeasible sends
  ROUTE_REQUEST for that origin, with its feasibility seqno plus one, to the
  neighbour it would use. A node holding a route at least that new answers
  with it, and only the origin advances its seqno to answer. Otherwise, while
  the hop count is at least two, it forwards the request with the hop count
  decremented, never to the requesting neighbour (RFC 8966 s3.8.1.2): through
  a feasible route's next hop if one avoids the requester, else through an
  infeasible route's, else it drops the request. A node keeps one request
  per origin and seqno pending, suppressing duplicates, and resends its own
  a small number of times; the pending entry expires when answered, when the
  route is gone, or when the retries run out, and only a later infeasible
  update starts a new one. Feasibility is never
  reset to accept a longer route after a retraction or a session replay.
  The handoff proof must revise this pending/retrigger sketch if a quiet
  origin could otherwise remain starved after the final retry.
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
  bump proves feasibility recovery, not the request exchange or its latency.
- Claim authority and freshness follow the handoff proof, independently of
  the requested path's forwarding choice. Next-hop authority alone failed
  the simulator below. A complete old snapshot cannot overwrite a known
  withdrawal. Do not preclude claim revisions or freshness floors before
  that proof. Store only state justified by its bounded replay/coverage model.
- Only the first hop selects the origin. It keeps `route_order`'s shape over
  the origin nodes announcing the path: longest prefix, newest epoch, the
  link's priority (`origin::Producer::with_preference`), seed plus route
  metric, then a rendezvous hash of the path and the identity, and the newest
  connection only for an otherwise identical route. The hash drops the hop
  chain, which differs per node, so every node agrees on a pool member
  (`pool_ties_agree_only_on_a_path_and_identity_hash`). The request then names
  that identity, and every later hop follows its own route to it, so
  forwarding needs a route but not the claim.
- Reselecting the origin at every hop loops although each origin's routes are
  loop-free (RFC 8966 s2.7): a ring of same-epoch importers whose relays
  prioritize different links, and nested wildcards where a relay has not
  heard the specific claim, both cycle past requester exclusion; joining
  another relay's subscription by path or epoch deadlocks; and without an
  epoch a later hop splices another publisher under the requester's
  announcement with no Restart (`local_priorities_loop_path_only_requests`,
  `overlapping_wildcards_loop_where_specific_claims_are_unheard`,
  `aggregation_cycles_starve_without_a_forwarding_constraint`,
  `epochless_reselection_splices_another_publisher`,
  `sweep_forwarding_progress`). A relay therefore joins a forwarded request
  only to a subscription following its route to the same identity. When that
  identity becomes unreachable the forwarded request ends, and the first hop
  re-resolves, resuming only on a matching epoch. A request carrying a cost
  bound instead also never loops, but stalls where a transit hop has not
  heard the claim and still splices epochless sources.
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
  subject-qualified identities rather than a downstream mark
  (`misconfigured_uplink_is_contained_by_qualified_identities`).
- Decided 2026-10-14: a local per-connection `proxy` preference decides
  whether a connection is sent routes learned from other connections; it is
  never on the wire. Relays default on; applications choose per P2P
  connection; browser tabs stay off until the Rust-to-TypeScript port.
- A plain client (`proxy` off) advertises its own origin and publications only. Its
  authenticated routing identity is shared across its live sessions so P2P
  can recognize the same origin, superseding the session-only identity plan.
  Clients receive only authorized, interested claims; opaque ids expose no
  backbone addresses, but are not themselves authorization. A client
  advertises nothing it receives, so it holds no feasibility distance and
  falls back from P2P to the CDN at once.
- Mixed versions: a lite-06 peer keeps today's path vector, translated at the
  relay that speaks both, for the rollout window only.

Why not a hold-down: #4644 retracted at once and re-announced a replacement
after 1 s, which ended a downstream front whose only path ran through the
relay, dropping subscribers for about a second while the relay kept serving;
holding without retracting brought hunting back in measurements, and
#4642's 300 ms cursor hold was the earlier mitigation. Main also contains
#5375's two-second retraction quarantine and #5391's same-clock queue dedup.
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

Open, for the implementer to settle and record:

- The earlier simulator persisted seqnos across restart. Re-run with the
  settled fresh routing incarnation and the stale-state rules above; do not
  carry the old assumption into acceptance.
- The window where an origin's ANNOUNCE_END and a next-hop change cross in
  flight, and what makes a replacement next hop's announce set current. Kept
  open: [Next-hop handoff](/quest/m1/cluster-routing/handoff.md) settles it
  with deterministic acceptance tests before the wire is written.
- How much of today's route trie, fronts, and `route_order` survives intact.

The #4349 counterexamples are the `residual_*` tests in
`rs/moq-net/tests/mesh_withdraw.rs`. They pin today's hunting on lite-06:
a stale path outlasting the 300 ms hold, a revival the quarantine misses,
and 179 hunting relays (3 reviving) over 200 seeded meshes. Turn each into a
single retraction everywhere, and keep `cross_node_close_retracts_once`
passing. On `main` an epochless switch to a stale path is a Restart, which a
lite-06 client sees as a retraction and an announcement; the release sent an
in-place update.

Publishing toggles too, decided 2026-10-13: add a time-mocked fixture where,
on some rings, a longer path arrives first and the better route then replaces
it as a Restart, so a lite-06 client sees `Start, End, Start` while the
broadcast stays live. Pin today's sequence beside the `residual_*` tests;
the epochless Restart rule above decides what it becomes.

Tests, time mocked: #4644's live-graph withdrawal and failover tests
(`quest/m0/path-hunting` branch) as acceptance, run on the wip version; a
link flap with many broadcasts behind one origin costs one ROUTE change per
link; a ring of three where a stale neighbour cannot revive an ended
broadcast; a two-uplink mesh that never carries CDN routes back to the CDN;
an island reached through the CDN after a partition; an unknown Route ID
refused; a five-node ring whose preferred origin link fails recovers its
longer backup through a seqno request, duplicates suppressed, while that
backup never loses its session; a ring where a node's own preferred route
points back at the requester forwards the request around it instead, and an
unanswerable request stops after its retries. Failover is measured
on broadcast liveness while the next hop changes, including a new next hop
whose ROUTE arrives before its ANNOUNCE set, and reports per-path
retractions and re-announcements beside the ROUTE budget; eventual route
recovery alone does not show it is seamless.

Tiered-cluster and partition/healing end-to-end checks belong in this quest,
not after lite-07 can be finalized: two regions with redundant cores, publish,
claim withdrawal, core loss, and matching-epoch failover reaching a far edge;
a two-uplink mesh reached through CDN during partition. Include overlapping
wildcards, a hidden exportable alternative under one selection, epochless Restart, and a
grant union crossing the scoped boundary each way (a `**` token added, then expiring). Fixtures are
public and time-mocked, wired into CI alongside Rust/JS interop.

Benchmark origins, broadcasts per origin, neighbors, active tracks/fronts,
and announce cursors independently. A route update must not expand back into
per-broadcast table rewrites or poll unrelated fronts. Measure messages, bytes,
wakes/select calls, CPU, replay work, and retained state. One broadcast per
origin is an explicit case. Coordinate with Route wakes and Announce replay;
benchmark smoke alone does not enforce complexity, so add operation-count
regressions. Bound pending requests and freshness state under same-clock churn.

Wire: `drafts/draft-lcurley-moq-lite.md` in the same PR, and `js/net`
encodes, decodes, and resolves it. Requests forwarded between routing nodes
name the serving identity (decided 2026-10-13). ROUTE carries one down-only
bit and no export metric. Browsers remain non-transit; P2P reuses this model
rather than adding a browser forwarding engine. Public API: the route-change
surface on `broadcast::Route` and its bindings will likely change, `Hops`
and saturating `Cost` go, zero-cost links are refused, a per-connection
`proxy` preference is added, and `cluster.id`/`--cluster-id` is removed
rather than kept as an unused label (decided 2026-10-13). `Preference`
continues to rank feasible transit routes; report it. This may
split at start (Rust and draft, then JS), as long as both land in one
release.

## Required

- [Identity proof](/quest/m1/cluster-routing/identity.md) - proven own identities and subject-qualified forwarded ones
- [Next-hop handoff](/quest/m1/cluster-routing/handoff.md) - complete and fresh claims with bounded state

## Closes

- [#4349](https://github.com/moq-dev/moq/issues/4349) - close once the `residual_*` tests in `mesh_withdraw.rs` retract once everywhere

## Related

- [Route wakes](/quest/m2/route-wakes.md) - index work follows the settled source-selection model
- [Announce replay](/quest/m2/perf/announce-replay.md) - linear initial/reconnect replay
- [Route health](/quest/m2/route-health/README.md) - epoch-only progress observation and local delivery failover

