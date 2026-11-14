# Cluster routing

## Goal

One routing model serves CDN tiers, outbound-connected local clusters, and
application-selected P2P connections. Topology changes update reachability
per affected origin rather than replaying every advertisement behind it.
Known advertisements remain distinct from current reachability, and stale
copies cannot revive a claim after its withdrawal is known.

Reliability, federation, and P2P are independently shippable milestones of
this architecture. Active/passive delivery health is out of scope; its
plan was deleted 2026-11-09 for want of a consumer.

## Plan

Decided with the maintainer, 2026-10-13:

- Keep per-origin ROUTEs and path-less ANNOUNCEs on the ordered announce
  stream in lite-07-wip. Reachability and origin-owned claims have separate
  lifetimes. A wildcard claim is a capability, not a list of live broadcasts;
  withdrawing one origin's claim does not end other origins' copies of an epoch.
- Applications choose connections and assign each a local priority, whether
  it reaches a CDN or a peer. Among Babel-feasible routes, priority precedes
  additive cost; feasibility can override preference, and the resulting
  arrival-order dependence is accepted. Link costs are strictly positive
  (default one per link): zero is refused, overflow is unreachable, and there
  is no hop-count component. Preserve deterministic path hashing among equal
  routes and newest-first only for otherwise identical routes. A new
  connection alone does not outrank a different equal-cost route. Cost
  includes the origin's prefix seed, which may be zero.
- The first hop selects the origin; forwarded requests name that identity
  and aggregation preserves it.
- GOAWAY gives the peer lowest preference and fails over to an eligible
  alternative, keeping it as a last resort while connected. Drain is a
  preference state, never a saturated metric.
- Keep Babel. A detour longer than the lost route may wait for a sequence
  refresh from the origin; instant failover is not required. Measure failure
  detection, additional routing recovery, and total interruption separately.
  Reopened 2026-11-11 (maintainer, /quest-plan after
  [moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487)'s reviews):
  the distance refuses a longer way round until the publisher advances its
  seqno, and refuses a cached copy at a neighbour no closer to the
  publisher, which [cache-aware routing](/quest/m3/cache-routing/README.md)
  wants to join. Decided the same day: a route is shared per publisher and
  per scope, and a relay believes a route by that route's own path. Rejected:
  link-state among relays, and routes to relays in place of publishers.
  Decided 2026-11-11 (maintainer, on the measurements of
  [moq-dev/moq#8533](https://github.com/moq-dev/moq/pull/8533)): Babel goes. A route is
  kept free of loops by its path, a withdrawal that names the hop that died,
  holds, and a timed doubt
  ([routes epic](/quest/m2/cluster-routes/README.md) Plan item 27). A
  detour no longer waits on the publisher, and today's router keeps Babel
  until [the switch](/quest/m2/cluster-routes/switch.md) lands. From
  then, "feasible" in this Plan reads as: the route's path does not name the
  relay and no doubt keeps the relay off it.
- An origin declares its own routing identity, fresh per process or tab
  restart and shared across its live sessions. It is unverified. Decided 2026-11-02 (maintainer, /quest-unblock): no
  key proof is planned; if a use case needs a provable identity, moq-lite-07's
  route identity becomes a public key, independent of P2P. Decided 2026-11-04
  (maintainer, /quest-unblock): scoped forgery is that use case. A peer proves
  a route key in its lite-07 SETUP, bound to the session, and every identity it
  sends while scoped, its own included, is qualified under it.
  Receiver-stamped
  per-session identities were dropped 2026-10-26 (maintainer): in a mesh,
  two relays stamping their sessions from one origin forward its routes as
  two identities a third relay cannot recognize as one. Keep administrative
  identity separate; `cluster.id` is removed. Switching between paths
  resumes only across a matching explicit epoch.
- Every identity a connection sends, its own and those it forwards, is
  qualified when its publish grant is scoped, and stays bare when it is
  unscoped, so a forger only damages routes inside its own scope. (Decided
  2026-10-14; own identities included 2026-10-26, since they are no longer
  proven.) The issuer-qualified subject key was dropped 2026-11-02
  (maintainer); since 2026-11-04 a scoped peer's identities are keyed by the
  route key it proves ([moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)).
- Matching explicit epochs authorize resume. Epochless changes retain
  Restart and fresh resolution; neither node identity nor a healthy
  alternate grants permission to splice content.
- During uncertain handoff, preserve known advertisement presence while
  representing unavailable forwarding separately. Never manufacture an END
  from route loss, imply successful delivery while unreachable, or retain
  unbounded stale state.
- One route selection per identity. Each node advertises its best route,
  with or without demand. Keep the `upstream` link mark and its down-only bit
  so CDN relays never shortcut between each other through a local network;
  CDN content still flows into the network and local content out, including
  through a peer whose own uplink died. A cheaper exportable alternative
  hidden behind a preferred down-only route is accepted; rank local peers
  above a backup uplink to avoid it. The relay that accepted a request from
  an upstream link ends that forwarded subscription when its route turns
  down-only; no request bit on the wire. (Decided 2026-10-14, superseding
  the two-view export plan.)
- A local per-connection `proxy` preference decides whether a connection is
  sent routes learned from other connections. Relays default on;
  applications choose per P2P connection (on for redundancy, off for
  conference offload). Browser tabs stay off until the
  [Rust-to-TypeScript](/quest/m5/rs2ts-js.md) port. Applications
  explicitly select P2P connections; discovery does not dial everyone.
- Each CDN broadcast enters a local network once by configuration: local
  peers outrank a backup uplink, and origin aggregation joins the rest.
- The deferred P2P consumer owns proving that its CDN session and discovery
  stay connected and unused CDN media demand is released after a handoff.
  m1 provides and tests the shared routing primitives without requiring an
  actual P2P integration.

The handoff proof showed a complete neighbor snapshot does not prove its
claims are current; receivers merge origin-versioned views by floor instead.
The routing contract is recorded in [Routes and announces](/quest/m2/cluster-routes/README.md)
with deterministic counterexamples: path-only subscriptions that reselect
origins at every hop loop and deadlock, so forwarded requests name their
identity; feasibility makes transit priorities depend on arrival order;
export keeps one selection with a down-only bit; and scoped forwarding is
contained by qualified identities.

The new routing model and finalized lite-07 belong to the breaking release.
Existing mesh deployments remain supported; the full edge/core topology
migration is independent. Basic outbound federation proves the common routing
contract without requiring that deployment migration.

Recent baseline: [moq-dev/moq#5375](https://github.com/moq-dev/moq/pull/5375)'s
two-second retraction quarantine reduced stale revivals but left residual
cases; [moq-dev/moq#5391](https://github.com/moq-dev/moq/pull/5391) bounded
its queue under repeated same-clock churn.
[moq-dev/moq#5382](https://github.com/moq-dev/moq/pull/5382) and
[moq-dev/moq#5393](https://github.com/moq-dev/moq/pull/5393) separately
reduced idle-session polling.
The 300 ms update hold alone is no longer the baseline. Keep regression
coverage for announcement churn, idle work, active continuity, and retained
state separately; fewer control bytes does not prove cheaper runtime work.

The earlier simulator estimates remain in [Routes and announces](/quest/m2/cluster-routes/README.md).
They do not prove the handoff correct; its fixtures do. Preserve public synthetic
fixtures and controlled-time regressions, including residual stale revival,
not only results from deployment tooling. Do not claim per-origin aggregation
reduces origin count when every application publishes one broadcast.

Non-goals: measured RTT/loss pricing by default, TURN, browser transit, full
link-state topology, or cross-operator economic exchange rates. Basic
federation does not wait for
administrative routing cost domains, which nobody plans. Node routes here
are the cold cost that [cache-aware routing](/quest/m3/cache-routing/README.md)'s
per-broadcast warm routes rank against (decided 2026-11-08, maintainer).

Each implementation updates the docs it makes stale. The separate
[routing guide](/quest/m1/routing-guide.md) explains the combined design and
setup after the capabilities land.

## Required

- [Routes and announces](/quest/m2/cluster-routes/README.md) - shared routing contract and implementation

## Related

- [P2P](/quest/m4/p2p/README.md) - application-selected peer sessions feed the shared routing model
- [Routing guide](/quest/m1/routing-guide.md) - concept page and federation setup; P2P extends it in m4
- [Route wakes](/quest/m4/route-wakes.md) - local work follows affected fronts rather than the whole covered table
- [Link quality](/quest/m5/link-quality.md) - optional measured link cost, distinct from delivery-health failover
- [Cache-aware routing](/quest/m3/cache-routing/README.md) - per-broadcast warm routes ranked against these node routes
