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
2026-10-13 [decisions](/quest/m1/cluster-routing/README.md). The sketch below
is conditional on Routing contracts and Next-hop handoff; those planning
quests settle identity, forwarding progress and claim freshness before coding:

- Wire sketch, flexible in detail. ROUTE_START carries the node id, a seqno,
  the metric, and a down-only bit, and implicitly takes the next stream-local
  Route ID, as ANNOUNCE_START takes the next Announce ID today. ROUTE_UPDATE
  (Route ID, seqno, metric, down-only bit) and ROUTE_END (Route ID) reference
  it; the bit rides UPDATE so a route moving between a mesh path and a CDN
  fallback changes it without retracting its announces.
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
- Use configured additive costs with hop-count defaults and a strictly
  increasing feasibility metric. Routing contracts settles zero-cost and
  overflow representation before this wire is fixed, replacing the earlier
  unqualified cost-plus-one sketch.
- Fresh opaque routing identity per process/tab restart, shared across its
  authenticated live sessions, decided 2026-10-13. Keep administrative
  identity and broadcast epoch separate. The shared identity recognizes an
  origin reached through CDN and direct links and maps it to a roster peer;
  a session cannot assert another origin's identity or seqno without authority.
  Follow Routing contracts for `cluster.id`, admission, and stale incarnations.
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
- Claim authority and freshness follow the handoff proof, independently of
  the requested path's forwarding choice. Next-hop authority alone failed
  the simulator below. A complete old snapshot cannot overwrite a known
  withdrawal. Do not preclude claim revisions or freshness floors before
  that proof. Store only state justified by its bounded replay/coverage model.
- Origin selection keeps `route_order`'s shape and swaps its inputs: the
  candidates become the origin nodes announcing the path, ranked by longest
  prefix, then the newest epoch, then the link's preference
  (`origin::Producer::with_preference`), then the
  sum of prefix seed and route metric, then the deterministic path-keyed
  rendezvous hash. Prefer the newest connection/announcement only for an
  otherwise identical route, preserving reconnect behavior without making
  every new equal-cost connection win. Apply the proved requester/export
  constraints first. Epochless source changes never resume.
- Down-only bit: set on a route learned on an upstream link, kept across
  other links, and a route carrying it is never sent on an upstream link
  (extending the `upstream` link mark in `doc/bin/relay/cluster.md`).
- A plain client advertises its own origin and publications only. Its
  authenticated routing identity is shared across its live sessions so P2P
  can recognize the same origin, superseding the session-only identity plan.
  Clients receive only authorized, interested claims; opaque ids expose no
  backbone addresses, but are not themselves authorization.
- Preserve path-only subscription aggregation if Routing contracts proves
  it safe under local priorities and multiple origins. Per-origin Babel
  feasibility is not a proof for arbitrary origin reselection at each hop.
  Implement the reviewed forwarding constraint; do not silently remove
  requester exclusion or assume aggregation makes a cycle deliver media.
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
  settled fresh routing incarnation and the contracts proof's stale-state
  rules; do not carry the old assumption into acceptance.
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
wildcards, per-export-view alternatives, and epochless Restart. Fixtures are
public and time-mocked, wired into CI alongside Rust/JS interop.

Benchmark origins, broadcasts per origin, neighbors, active tracks/fronts,
and announce cursors independently. A route update must not expand back into
per-broadcast table rewrites or poll unrelated fronts. Measure messages, bytes,
wakes/select calls, CPU, replay work, and retained state. One broadcast per
origin is an explicit case. Coordinate with Route wakes and Announce replay;
benchmark smoke alone does not enforce complexity, so add operation-count
regressions. Bound pending requests and freshness state under same-clock churn.

Wire: `drafts/draft-lcurley-moq-lite.md` in the same PR, and `js/net`
encodes, decodes, and resolves it. Browsers remain non-transit; P2P reuses
this model rather than adding a browser forwarding engine. Public API: the
route-change surface on
`broadcast::Route` and its bindings will likely change; report it. This may
split at start (Rust and draft, then JS), as long as both land in one
release.

## Required

- [Routing contracts](/quest/m1/cluster-routing/contracts.md) - authenticated incarnation, local priorities, forwarding progress, and export views
- [Next-hop handoff](/quest/m1/cluster-routing/handoff.md) - complete and fresh claims with bounded state

## Closes

- [#4349](https://github.com/moq-dev/moq/issues/4349) - close once the `residual_*` tests in `mesh_withdraw.rs` retract once everywhere

## Related

- [Route wakes](/quest/m2/route-wakes.md) - index work follows the settled source-selection model
- [Announce replay](/quest/m2/perf/announce-replay.md) - linear initial/reconnect replay
- [Route health](/quest/m2/route-health/README.md) - epoch-only progress observation and local delivery failover

