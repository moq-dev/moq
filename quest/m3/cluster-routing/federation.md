# [L] Outbound local-cluster federation

## Goal

A local relay behind an inbound firewall dials a CDN relay and exchanges all
authorized broadcast claims and demanded media in both directions. Local
paths and CDN paths use the shared priority/cost rules, including a local
cluster with two uplinks, partition, and healing. No bespoke federation router.

## Plan

Reopened 2026-11-15 (maintainer, /quest-plan on
[cache-aware multicast](/quest/m2/multicast/README.md), item 3): claims, not
routes, cross a trust boundary. The CDN relay scopes the local relay's
connection to a path prefix, so the session carries claims alone (the
lite-07 draft's Routing: a peer confined to part of the tree exchanges no
routes). The CDN relay announces each claim it takes there under its own
Node ID with a chain of the relays that did so, and refuses a claim whose
chain names it. No route, qualified identity, or route key crosses the grant,
so a forger declaring a victim's identity captures nothing: the claim
belongs to the relay that announced it again. Nested scoped federation is in:
a relay behind the local relay may be scoped too, and the chain bounds the
nesting at 32 relays. The route key, qualified Node IDs, and their key file
went with [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987);
[claim chain](/quest/m2/multicast/claim-chain.md) changes what the chain
records before the cut.

Decided 2026-10-13: this is a required use case, independently shippable from
peer-to-peer routing. Existing cluster sessions are bidirectional and scoped peer
admission exists; extend and prove those paths rather than creating a reverse
connection protocol. Do not require an inbound connection to the local relay.

The local relay marks its CDN links `upstream`, the draft's marked sessions,
so a claim or route learned over one never goes out another and CDN relays
never shortcut through the local network; it keeps its local `proxy`
preference on toward the CDN. At equal rank an unmarked route beats a marked
one, but a strictly cheaper marked route still wins, so an operator who wants
local peers ahead of a backup uplink gives them a higher priority
(multicast item 13; decided 2026-10-14 as one selection with the cheaper
exportable alternative accepted). Two uplinks to two CDN relays are two
claims of one Instance, one per CDN relay, so a reader on the CDN moves
between them without a restart when one is lost.

CDN content still flows into the local network and local content out: a local
relay whose own uplink died reaches the CDN, in both directions, through a
local peer. Each CDN broadcast enters the local network once by
configuration: local peers outrank a backup uplink, and origin aggregation
joins the rest (decided 2026-10-14).
Basic federation requires neither economic cost translation nor cross-operator cost domains.

Controlled-time integration tests: publish/consume in both directions through
one outbound session; two local uplinks without CDN transit through the LAN;
local partition reached through CDN, then restoration of the cheaper eligible
local route; overlapping namespace grants, a claim outside the grant refused,
and a claim that comes back to a relay its chain names refused; a scoped
relay behind the scoped local relay; restart and retained neighbor state; a
CDN broadcast watched on both local relays crossing one uplink once, failing
over when that uplink dies; a local relay with a dead uplink publishing and
consuming through its peer. Test a move between claims of one Instance and a
restart under a new Instance separately. A client may retain a CDN session
while consuming over a better authorized local connection, with no unused
duplicate media demand.

Measure origins/broadcasts and peers/readers independently under reconnect
and uplink churn. Reuse route-layer fixtures and ensure these run in CI.
Update `doc/bin/relay/cluster.md` (`proxy`, `upstream`, and the
one-copy-per-broadcast priority recipe) and affected auth/config examples in
the same PR; the combined concept/setup guide follows separately.

Decided 2026-11-06 (maintainer; the agent picked the children): require
only the route children federation builds on, session integration (landed
in [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023) and
[moq-dev/moq#8293](https://github.com/moq-dev/moq/pull/8293)) and
[acceptance](/quest/m2/cluster-routes/acceptance.md), not the whole
[route epic](/quest/m2/cluster-routes/README.md). Federation is
relay-to-relay: its consumer, Kixel's mesh relay node (downstream, in
kixel-ai/kixel.ai), is a Rust relay too. Rejected: session integration
alone, which drops the proven contract the wire rule below relies on.

Public API: report any peer configuration or admission change. Wire: the
lite-07 draft's Claims and Routing as they ship; any change requires draft
and Rust/JS parity. Run `just check`, `just test interop --all`, and `just drafts check`.

## Required

- [Route layer acceptance](/quest/m2/cluster-routes/acceptance.md) - the forwarding and partition fixtures this reuses, proven on the implementation

## Related

- [Routing guide](/quest/m3/routing-guide.md) - runnable operator setup after integration
- [Claim chain](/quest/m2/multicast/claim-chain.md) - what a claim's chain records across each grant
