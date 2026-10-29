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
`rs/moq-net/tests/integration/routing_contracts/` and run against a small reference model;
port each topology to the implementation as acceptance, then delete the model.
The handoff contract below, decided 2026-10-16, settles claim freshness:

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
  Route ID is a protocol violation. A ROUTE_UPDATE with an infinite metric
  (2^64-1, the largest lite-07 varint; a sum reaching it is unreachable)
  means held: the sender has no usable route but still holds the origin's
  announcements while it recovers. ROUTE_END means the sender let go of the
  origin on this stream; neither is proof that the origin withdrew its
  claims (see the handoff contract). Each batch of ANNOUNCEs for a route
  closes with one ANNOUNCE_FLOOR listing every region the sender certifies
  for that route: a count, then per region a suffix of the stream's interest
  prefix (empty for the whole interest) and the origin revision as of which
  the stream's set for that route in that region is complete. The list
  replaces the previous one, so a region left out is no longer certified,
  and a count of zero answers a stream whose interest none of the sender's
  regions meets.
  ROUTE_REQUEST (node id, seqno) travels
  the other way on the same stream, toward the neighbour that sent the route,
  and so does the echo of every ROUTE_END: the receiver sends the same
  ROUTE_END (Route ID) back once it has dropped the route, which tells the
  sender no neighbour still holds its advertisement (see the handoff
  contract). Every receiver echoes, clients included; a closed session
  counts as the echo.
  The hop list leaves lite-07, and with it the
  `Hop Base`/`Hop Keep` compression; `cluster.max_hops` then stops applying
  to lite-07. Most of the 62% saving measured in
  [moq-dev/moq#4196](https://github.com/moq-dev/moq/pull/4196) came from hop
  chains, so rerun its announce bench on path-less ANNOUNCE and drop
  `Path Base`/`Path Keep` too unless it still pays for its rules.

  ```text
  ROUTE_START  node=0x7a3f seqno=41 metric=12   -> route 0
  ANNOUNCE_START prefix=transcode/ route=0 cost=10    -> announce 0
  ANNOUNCE_START prefix=transcode/foobar route=0 cost=1 -> announce 1
  ANNOUNCE_FLOOR route=0 count=1 ""@7           # complete as of revision 7
  ROUTE_UPDATE route=0 seqno=42 metric=15
  ANNOUNCE_END announce=1                       # this origin withdraws a claim
  ANNOUNCE_FLOOR route=0 count=1 ""@8
  ANNOUNCE_FLOOR route=1 count=2 "live/a/"@3 "live/b/"@5  # a scoped key
  ANNOUNCE_FLOOR route=1 count=1 "live/b/"@6    # live/a/ is no longer certified
  ROUTE_UPDATE route=0 seqno=42 metric=inf      # held: unusable, still recovering
  ROUTE_END    route=0                          # let go: forget it on this stream
  ROUTE_END    route=0                          # <- the receiver echoes it back
  ```
- Scope route state to interested origins, not the CDN's whole node table.
  Send a route before any announcement referencing it. The handoff contract
  defines held routes, letting go, and collection.
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
  shared across its live sessions, decided 2026-10-13. It is unverified:
  recognizing a routing identity needs key proof, which
  [Peer grants](/quest/m2/p2p/peer-grant.md) owns (decided 2026-10-25).
  Decided 2026-10-26 (maintainer): receivers do not stamp a fresh identity
  per session. In a mesh, two relays stamping their sessions from one origin
  forward its routes as two identities a third relay cannot recognize as
  one, and a declared identity kept beside the stamp as a loop hint would
  stay unverified.
  Switching between paths resumes only across a matching explicit epoch.
  Keep administrative identity and broadcast epoch separate. The origin
  advances its seqno whenever its own set of sessions changes: feasibility
  distances held elsewhere otherwise refuse a restart that reuses an
  identity, and starve the old neighbors of an origin that moved to a
  farther attachment (`restart_reusing_identity_is_refused`,
  `reconnect_without_restart_needs_a_seqno_advance`). A random identity alone
  does not keep a dead incarnation away. What bounds it is ordered sessions,
  state scoped to the session that delivered it, never advertising an
  unreachable route, and keeping the feasibility distance until every
  ROUTE_END it sent is echoed back: a collected incarnation then reappears
  only from a
  message already in flight on a live session, which delivers the retraction
  next (`collected_incarnation_returns_only_while_its_retraction_is_in_flight`).
  `cluster.id` stops naming a routing identity.
- Decided 2026-10-14: every identity a connection sends is subject-qualified:
  filed under `hash(subject, id)` when the connection's publish grant is
  scoped, and bare when it is unscoped (publish `**` at the root). Its own
  identity is no exception (2026-10-26): the bare exemption relied on the
  dropped proof, so an unproven own identity could otherwise claim a
  victim's. Extend the fixture below to a forger declaring the victim's id as
  its own, and drop the reference model's `proved` exemption. The hash
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
  loop-free (RFC 8966 s2.7): a ring of same-epoch origins whose relays
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
- A plain client (`proxy` off) advertises its own origin and publications only, under
  the one identity it declares on every live session. Clients receive only authorized, interested claims; opaque ids expose no
  backbone addresses, but are not themselves authorization. A client
  advertises nothing it receives, so it holds no feasibility distance and
  falls back from P2P to the CDN at once. It still echoes every ROUTE_END.
- Mixed versions: a lite-06 peer keeps today's path vector, translated at the
  relay that speaks both, for the rollout window only.

Handoff contract, proven 2026-10-15 by the fixtures in `handoff.rs` against
next-hop authority and a snapshot barrier alone. A next-hop change adopts the
newest knowledge any session has, never an older complete snapshot, and route
loss never reports an END:

- Each origin numbers the changes to its own claims with a revision that
  starts at zero for each routing identity, separate from the route seqno.
  Both are varints. An origin never advertises the last value, 2^64-1 (the
  largest lite-07 varint, as for the infinite metric): a
  seqno advance, claim change, or seqno request that would reach it rotates
  the identity instead, ending the old one everywhere and announcing the
  claims again under a fresh one, so nothing wraps and every advertised
  seqno leaves room for the successor a request names
  (`exhausted_counters_rotate_the_identity`).
  A session sends an origin's announcements in batches, each closed by its
  floors: per region, the revision as of which the stream's announcements
  for that route within the region are complete. A bare identity's region is
  the stream's interest. A subject-qualified key certifies only within the
  grant scopes it arrived through, and one key can arrive through grants
  whose pattern unions differ, so its view carries one floor per scope.
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
- A node tracks the regions each neighbour's last view certifies. Only a
  neighbour whose last finite advertisement is feasible here counts, held or
  not: feasible edges strictly decrease the metric toward the origin, so two
  nodes never keep a region alive through each other. While the node has a
  usable route and every such neighbour has answered the current interest, a
  region of its view that none of them certifies is let go: its last
  certifier closed, ended its route, or no longer meets the interest. The
  reader sees those claims go offline, not end. The view keeps what it knows
  within the regions still certified when no lost floor is newer than every
  kept one, so a lagging certifier cannot revive a withdrawal the departed one
  delivered (`a_lost_scope_keeps_newer_knowledge_where_still_certified`).
  Otherwise a wildcard above both was last learned through the lost floor and
  is unknown at the kept floors' revision, so the view is rebuilt from the
  neighbours' last views, a collection of that region. A sender that
  certifies anything for a route answers every stream, even one whose
  interest none of its regions meets, so its peer learns it certifies nothing
  there rather than waiting (`a_disjoint_interest_is_answered_with_nothing`).
  A claim is available only through a certifier that is not held; another
  scope's usable route cannot serve it. A certifier that has not answered yet
  counts as serving, as for a new next hop whose announcements trail its
  route. Closing one of two grants
  under a subject takes its scope offline at the client
  (`a_closed_grant_lets_its_scope_go`), and a relay downstream of two scoped
  uplinks shows a lost scope unavailable while the starved relay holds, then
  lets it go once a request recovers a route
  (`a_relay_lets_go_of_a_scope_its_uplinks_lost`). An interest that meets no
  grant scope certifies nothing, so its claims go offline
  (`sweep_scoped_handoff`). Without this, the closed grant's claims stay
  shown and available indefinitely, since no other grant can deliver their
  withdrawal.
- Per identity, a node is usable (a feasible selected route: announcements
  shown available), held, or let go. It is held while starved, or while a
  neighbor's held route has a last finite advertisement feasible here; it
  keeps showing the announcements as unavailable and sends ROUTE_UPDATE with
  an infinite metric. Holds therefore follow strictly decreasing metrics
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
  accepting it loops; once all echoes are back no node holds its
  advertisement, so dropping the distance is as safe as a fresh node joining
  (`collection_keeps_the_feasibility_distance_against_a_queued_echo`). The
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
2. Held encoding: ✅ an infinite metric (2^64-1) means held and ROUTE_END
   means let go, with no new field. Rejected: an explicit held bit.
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
   (`sweep_forgetting_distances`). Rejected: keeping the distance forever,
   which grows with publisher churn times uptime on every relay that saw an
   identity, about 16 bytes each, until a restart; and Babel's source GC
   timer, which assumes stale echoes are gone by then rather than proving it.
7. Scope coverage under one subject: ✅ per-region floors. ANNOUNCE_FLOOR
   names the region it certifies, a suffix of the stream's interest prefix
   (the scope met with the interest), each node tracks which regions each
   feasible neighbour's last view certifies, and a region none certifies
   goes offline. One key can arrive through grants whose pattern unions
   differ, and without this a scope whose last session closed stays shown,
   available, with no way to learn its withdrawal; a relay forwarding such a
   key also needs one floor per scope on a stream. Rejected: one pattern
   union per subject, which refuses or re-files a refresh that changes the
   union and cannot be checked across relays; and deferring to the
   implementation, which leaves the proof's freshness claim false for scoped
   delivery.

8. Region-set framing: ✅ one ANNOUNCE_FLOOR per batch lists every region
   the sender certifies for the route: (Route ID, count, then a region suffix
   and revision per region). The list replaces the last, so a sender stops
   certifying a region it lost by leaving it out, and a count of zero answers
   an interest it cannot certify, which its peer needs to stop waiting
   (`a_disjoint_interest_is_answered_with_nothing`). The common case is one
   empty suffix, one byte over (Route ID, revision). Rejected: one region per
   ANNOUNCE_FLOOR with revision 0 dropping a region, which gives revision 0 a
   meaning and costs a floor per dropped region; and an end-of-batch marker,
   a message for what one floor frames.

Open, for this quest's implementation: any further wire detail of the region
list, such as its order, is settled there with Rust and JS interop, not in
the proof. So is whether letting go of a region keeps each still-certified
region's knowledge independently: the proof keeps them all or, if any lost
floor is newer than a kept one, rebuilds them all, a collection whose replay
is bounded like any other.

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

Open, for the implementer to settle and record:

- The earlier simulator persisted seqnos across restart. Re-run with the
  settled fresh routing incarnation and the stale-state rules above; do not
  carry the old assumption into acceptance.
- How much of today's route trie, fronts, and `route_order` survives intact.

The counterexamples from #4349 (closed; this quest still owns the fix) are the `residual_*` tests in
`rs/moq-net/tests/integration/mesh_withdraw.rs`. They pin today's hunting on lite-06:
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

Tests, time mocked: the handoff fixtures and sweep in `handoff.rs`;
[moq-dev/moq#4644](https://github.com/moq-dev/moq/pull/4644)'s live-graph withdrawal and failover tests (the PR closed and
its branch is gone; fetch `refs/pull/4644/head`) as acceptance, run on the wip version; a
link flap with many broadcasts behind one origin costs one ROUTE change per
link; a ring of three where a stale neighbour cannot revive an ended
broadcast; a two-uplink mesh that never carries CDN routes back to the CDN;
an island reached through the CDN after a partition; an unknown Route ID
refused; a five-node ring whose preferred origin link fails recovers its
longer backup through a seqno request, duplicates suppressed, while that
backup never loses its session; a ring where a node's own preferred route
points back at the requester forwards the request to it, which carries it
on along its own route, and an unanswerable request stops once no usable
route is left. Failover is measured
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
rather than kept as an unused label (decided 2026-10-13). Announcements
expose availability, and a let-go is reported as offline rather than END
(decided 2026-10-16). `Preference`
continues to rank feasible transit routes; report it. This may
split at start (Rust and draft, then JS), as long as both land in one
release.

Decided 2026-10-27 (maintainer): [Grant subject](/quest/m1/cluster-routing/grant-subject.md)
split out of this quest, and this quest builds on it. It carries the
issuer-qualified subject the scoped keys above are named by, and fixes the
reference model's `proved` exemption.

Decided 2026-10-27 (maintainer): this quest deletes the epochless instance
machinery in `rs/moq-net/src/model/origin.rs`: `Instance::Route(generation)`
and its renewals, `restarted_since`, the held-restart Release for epochless
routes, and Selection retirement, leaving Follow's same-epoch
Update/Restart split as the only takeover rule. Once each origin is its own
ANNOUNCE and the first hop pins the request to its identity, the first hop's
route is the instance, so the per-hop renewal has nothing left to protect.
An epochless change of serving route stays a Restart, including the
same-origin next-hop change in the wire sketch above, and the routing
contract's epochless fixtures keep passing. Minting an epoch for every route
instead was tried and rejected
([moq-dev/moq#6948](https://github.com/moq-dev/moq/pull/6948)): each relay
mints at its own time, so mint order outranks cost, preference, and the
local-broadcast tie and relays disagree on a pool path's winner, while
ranking a minted epoch last keeps the renewal it was meant to delete.

Decided 2026-10-27 (maintainer), from
[grant subject](https://github.com/moq-dev/moq/pull/6992): decide what key
identities are filed under for sessions allowed to publish part of the tree
with no subject (public rules, mTLS certificates, or a third-party auth
server), and whether the relay refuses such a grant when the client
presented a token. Also from
[moq-dev/moq#6947](https://github.com/moq-dev/moq/pull/6947): in `js/net`'s
`origin.ts` a detached request stays on its own route instead of following
the cheapest one; fix it with the route-selection rework here.

## Required

- [Grant subject](/quest/m1/cluster-routing/grant-subject.md) - the issuer-qualified subject scoped identities are filed under

## Related

- [Route wakes](/quest/m2/route-wakes.md) - index work follows the settled source-selection model
- [Route health](/quest/m2/route-health/README.md) - epoch-only progress observation and local delivery failover

