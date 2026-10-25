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

Use authenticated, namespace-scoped admission: the CDN scopes the local
relay's connection to a path prefix, and identities it forwards for its local
publishers are filed under `hash(subject, id)`, keyed by the grant's
credential subject, so a forger or a misconfigured
uplink only affects its own scope (decided 2026-10-14; see
[Routes and announces](/quest/m1/cluster-routing/routes.md)). A scoped
connection cannot forward an already qualified identity, so nested scoped
federation is out: a relay behind the local relay connects unscoped. The local relay
marks its CDN links `upstream`, so CDN relays never shortcut through the
local network, and keeps its local `proxy` preference on toward the CDN. One
route selection per identity; a cheaper exportable alternative hidden behind
a preferred down-only route is accepted (decided 2026-10-14).

CDN content still flows into the local network and local content out: a local
relay whose own uplink died reaches the CDN, in both directions, through a
local peer. Each CDN broadcast enters the local network once by
configuration: local peers outrank a backup uplink, and origin aggregation
joins the rest (decided 2026-10-14).
Basic federation requires neither economic cost translation nor cross-operator cost domains.

Controlled-time integration tests: publish/consume in both directions through
one outbound session; two local uplinks without CDN transit through the LAN;
local partition reached through CDN, then restoration of the cheaper eligible
local route; overlapping namespace grants and rejected identity/scope spoofing;
restart and retained neighbor state; a CDN broadcast watched on both local
relays crossing one uplink once, failing over when that uplink dies; a local
relay with a dead uplink publishing and consuming through its peer. Test
equivalent epochs and epochless Restart separately. A client may retain a CDN session while consuming over a
better authorized local connection, with no unused duplicate media demand.

Measure origins/broadcasts and peers/readers independently under reconnect
and uplink churn. Reuse route-layer fixtures and ensure these run in CI.
Update `doc/bin/relay/cluster.md` (`proxy`, `upstream`, and the
one-copy-per-broadcast priority recipe) and affected auth/config examples in
the same PR; the combined concept/setup guide follows separately.

Public API: report any peer configuration or admission change. Wire: use the
proven lite-07 routing contract; any additional change requires draft and
Rust/JS parity. Run `just check`, `just test interop --all`, and `just drafts check`.

## Required

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - identity, scoped claims, reachability, and export policy

## Related

- [Route health](/quest/m2/route-health/README.md) - epoch-only detection of stalled delivery on still-open links
- [Routing guide](/quest/m1/routing-guide.md) - runnable operator setup after integration
