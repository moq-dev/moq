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
  it reaches a CDN or a peer. Priority precedes additive cost; at equal
  priority use the lowest eligible total cost, with hop-count defaults.
  Preserve deterministic path hashing among equal routes and newest-first
  only for otherwise identical routes. A new connection alone does not
  outrank a different equal-cost route. Cost includes the origin's prefix seed.
- Mint a fresh opaque routing identity per process or tab restart, shared
  across that live origin's authenticated sessions. Keep administrative
  identity separate. Session scoping alone cannot identify one publisher
  through both its CDN and P2P connections. Authentication must prevent a
  peer claiming another origin or advancing its sequence number.
- Matching explicit epochs authorize resume. Epochless changes retain
  Restart and fresh resolution; neither node identity nor a healthy
  alternate grants permission to splice content.
- During uncertain handoff, preserve known advertisement presence while
  representing unavailable forwarding separately. Never manufacture an END
  from route loss, imply successful delivery while unreachable, or retain
  unbounded stale state.
- Keep upstream/down-only export policy so two local uplinks do not make
  the local network CDN transit. Preserve longer exportable alternatives
  even when a cheaper down-only route wins locally.
- Browsers serve only their own publications. Relays and opted-in native
  routing nodes may forward received content. Applications explicitly
  select P2P connections; discovery does not dial everyone.
- Retain the CDN session and discovery during P2P use. Release unused CDN
  media demand, except for bounded overlap during a same-epoch handoff.

The implementation is gated on [routing contracts](/quest/m1/cluster-routing/contracts.md)
and [handoff proofs](/quest/m1/cluster-routing/handoff.md). Per-origin Babel
feasibility alone does not prove that path-only subscriptions which reselect
origins under local priorities make progress. Nor does a complete neighbor
snapshot prove its claims are current. Settle those contracts before dropping
hop-vector protections or fixing the wire.

The new routing model and finalized lite-07 belong to the breaking release.
Existing mesh deployments remain supported; the full edge/core topology
migration is independent. Basic outbound federation proves the common routing
contract without requiring that deployment migration.

Recent baseline: #5375's two-second retraction quarantine reduced stale
revivals but left residual cases; #5391 bounded its queue under repeated
same-clock churn. #5382 and #5393 separately reduced idle-session polling.
The 300 ms update hold alone is no longer the baseline. Keep regression
coverage for announcement churn, idle work, active continuity, and retained
state separately; fewer control bytes does not prove cheaper runtime work.

The earlier simulator estimates remain in [Routes and announces](/quest/m1/cluster-routing/routes.md).
They do not prove the proposed handoff correct. Preserve public synthetic
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

- [Routing contracts](/quest/m1/cluster-routing/contracts.md) - identity, local priority, loop freedom, and export views have a consistent tested model
- [Next-hop handoff](/quest/m1/cluster-routing/handoff.md) - complete and fresh claims survive rerouting without resurrection or false END
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - implement the proven route layer with Rust/JS parity and tiered-cluster acceptance
- [Outbound federation](/quest/m1/cluster-routing/federation.md) - an outbound-only local relay exchanges authorized broadcasts with the CDN and survives partition/healing

## Related

- [Route health](/quest/m2/route-health/README.md) - epoch-only active/passive delivery monitoring and automatic recovery
- [P2P](/quest/m1/p2p/README.md) - application-selected peer sessions feed the shared routing model
- [Routing guide](/quest/m1/routing-guide.md) - concept page and practical federation/P2P setup
- [Route wakes](/quest/m2/route-wakes.md) - local work follows affected fronts rather than the whole covered table
- [Link quality](/quest/m2/link-quality.md) - optional measured link cost, distinct from delivery-health failover
- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - independent origins serving the same explicit epoch
