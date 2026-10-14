# [L] Outbound local-cluster federation

## Goal

A local relay behind an inbound firewall dials a CDN relay and exchanges all
authorized broadcast claims and demanded media in both directions. Local
paths and CDN paths use the shared priority/cost rules, including a local
cluster with two uplinks, partition, and healing. No bespoke federation router.

## Plan

Decided 2026-10-13: this is a required use case, independently shippable from
browser P2P. Existing cluster sessions are bidirectional and scoped peer
admission exists; extend and prove those paths rather than creating a reverse
connection protocol. Do not require an inbound connection to the local relay.

Use authenticated, namespace-scoped peer admission. A normal publisher is not
implicitly an unrestricted cluster peer. Link direction in export policy is
independent of which side dialed; the local side's CDN uplink must not make
the CDN treat the local relay as its own upstream. Preserve the down-only
restriction across the local mesh and export eligible local alternatives.
Basic federation requires neither economic cost translation nor folded domain IDs.

Controlled-time integration tests: publish/consume in both directions through
one outbound session; two local uplinks without CDN transit through the LAN;
local partition reached through CDN, then restoration of the cheaper eligible
local route; overlapping namespace grants and rejected identity/scope spoofing;
restart and retained neighbor state. Test equivalent epochs and epochless
Restart separately. A client may retain a CDN session while consuming over a
better authorized local connection, with no unused duplicate media demand.

Measure origins/broadcasts and peers/readers independently under reconnect
and uplink churn. Reuse route-layer fixtures and ensure these run in CI.
Update `doc/bin/relay/cluster.md` and affected auth/config examples in the same
PR; the combined concept/setup guide follows separately.

Public API: report any peer configuration or admission change. Wire: use the
proven lite-07 routing contract; any additional change requires draft and
Rust/JS parity. Run `just check`, `just test interop --all`, and `just drafts check`.

## Required

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - identity, scoped claims, reachability, and export policy

## Related

- [Route health](/quest/m2/route-health/README.md) - epoch-only detection of stalled delivery on still-open links
- [Routing guide](/quest/m1/routing-guide.md) - runnable operator setup after integration
