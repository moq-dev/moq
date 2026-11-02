# Cluster routing

## Goal

One routing model serves CDN tiers, outbound-connected local clusters, and
application-selected P2P connections. Topology changes update reachability
per affected origin rather than replaying every advertisement behind it.
Known advertisements remain distinct from current reachability, and stale
copies cannot revive a claim after its withdrawal is known.

Reliability, federation, and P2P are independently shippable milestones of
this architecture. Active/passive delivery health is a separate
[route-health line](/quest/m2/route-health/README.md), shared by endpoints
and relays, and applies only to broadcasts with epochs.

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
- An origin declares its own routing identity, fresh per process or tab
  restart and shared across its live sessions. It is unverified: recognizing
  a routing identity needs key proof, which
  [Peer grants](/quest/m2/p2p/peer-grant.md) owns. Receiver-stamped
  per-session identities were dropped 2026-10-26 (maintainer): in a mesh,
  two relays stamping their sessions from one origin forward its routes as
  two identities a third relay cannot recognize as one. Keep administrative
  identity separate; `cluster.id` is removed. Switching between paths
  resumes only across a matching explicit epoch.
- Every identity a connection sends, its own and those it forwards, is filed
  under `hash(subject, id)`, keyed by the grant's credential subject, when
  its publish grant is scoped, and stays bare when it is unscoped. The key is
  deterministic, so two uplinks of one subject share it across token refresh,
  and a forger only damages routes inside its own scope. (Decided 2026-10-14;
  subject decided 2026-10-15; own identities included 2026-10-26, since they
  are no longer proven.)
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
  [Rust-to-TypeScript](/quest/m2/rs2ts-js.md) port. Applications
  explicitly select P2P connections; discovery does not dial everyone.
- Each CDN broadcast enters a local network once by configuration: local
  peers outrank a backup uplink, and origin aggregation joins the rest.
- The deferred P2P consumer owns proving that its CDN session and discovery
  stay connected and unused CDN media demand is released after a handoff.
  m1 provides and tests the shared routing primitives without requiring an
  actual P2P integration.

The handoff proof showed a complete neighbor snapshot does not prove its
claims are current; receivers merge origin-versioned views by floor instead.
The routing contract is recorded in [Routes and announces](/quest/m1/cluster-routing/routes.md)
with deterministic counterexamples: path-only subscriptions that reselect
origins at every hop loop and deadlock, so forwarded requests name their
identity; feasibility makes transit priorities depend on arrival order;
export keeps one selection with a down-only bit; and scoped forwarding is
contained by subject-qualified identities.

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

The earlier simulator estimates remain in [Routes and announces](/quest/m1/cluster-routing/routes.md).
They do not prove the handoff correct; its fixtures do. Preserve public synthetic
fixtures and controlled-time regressions, including residual stale revival,
not only results from deployment tooling. Do not claim per-origin aggregation
reduces origin count when every application publishes one broadcast.

Non-goals: measured RTT/loss pricing by default, cache-dependent route prices,
TURN, browser transit, full link-state topology, or cross-operator economic
exchange rates. Basic federation does not wait for
[routing cost domains](/quest/m3/routing-cost-domains.md).

Each implementation updates the docs it makes stale. The separate
[routing guide](/quest/m1/routing-guide.md) explains the combined design and
setup after the capabilities land.

## Required

- [Grant subject](/quest/m1/cluster-routing/grant-subject.md) - grants carry an issuer-qualified subject, scoped tokens without one are refused, and the routing model drops its `proved` exemption
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - implement the proven route layer with Rust/JS parity and tiered-cluster acceptance
- [Outbound federation](/quest/m1/cluster-routing/federation.md) - an outbound-only local relay exchanges authorized broadcasts with the CDN and survives partition/healing

## Related

- [Route health](/quest/m2/route-health/README.md) - epoch-only active/passive delivery monitoring and automatic recovery
- [P2P](/quest/m2/p2p/README.md) - application-selected peer sessions feed the shared routing model
- [Routing guide](/quest/m1/routing-guide.md) - concept page and federation setup; P2P extends it in m2
- [Route wakes](/quest/m2/route-wakes.md) - local work follows affected fronts rather than the whole covered table
- [Link quality](/quest/m2/link-quality.md) - optional measured link cost, distinct from delivery-health failover
