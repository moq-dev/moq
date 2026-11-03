# [L] Route layer acceptance

## Goal

The route layer is proven on the implementation, not the model: the
forwarding and first-hop selection fixtures run against the router and the
session, and the reference model in
`rs/moq-net/tests/integration/routing_contracts/` is deleted. The incident
fixtures, tiered-cluster and partition end-to-end checks, failover
measurements, and benchmarks below pass in CI, and the mitigations they
replace are retired.

## Plan

- Port the forwarding fixtures (`Forward`, `Aggregate`, pool ties, and the
  sweeps over them) to the implementation, then delete the model.
- The earlier simulator persisted seqnos across restart. Re-run with the
  settled fresh routing incarnation and the stale-state rules; do not carry
  the old assumption into acceptance.
- Retire [moq-dev/moq#5375](https://github.com/moq-dev/moq/pull/5375)'s
  quarantine, [moq-dev/moq#5391](https://github.com/moq-dev/moq/pull/5391)'s
  dedup, and the 300 ms hold only once the fixtures below demonstrate their
  replacement (see the epic's hold-down note).
- Add a transit fixture for routes scoped to interested origins (the
  session child's "scope route state to interested origins"). The router
  sends every route to every watching neighbor today, so a client is sent
  the CDN's whole node table. The fixture decides when a route must still
  go out to a stream whose interest meets none of the origin's claims:
  - a transit node forwarding requests for paths outside its own interest;
  - a node holding or forwarding a seqno request for that identity;
  - a sender whose certified regions miss the interest, which must still
    answer with an empty floor list
    (`a_disjoint_interest_is_answered_with_nothing`).

The counterexamples from #4349 (closed; this epic still owns the fix) are the `residual_*` tests in
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
the epic's epochless Restart rule decides what it becomes.

Tests, time mocked: the handoff fixtures and sweeps, already on the router;
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

Tiered-cluster and partition/healing end-to-end checks belong here,
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

## Required

- [Route session integration](/quest/m1/cluster-routing/routes/session.md) - the implementation under test
- [js/net route layer](/quest/m1/cluster-routing/routes/js.md) - the JS side of the interop checks

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes/README.md) - the contract and its fixtures
