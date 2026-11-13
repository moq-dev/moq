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

Decided 2026-11-12 (maintainer, /quest-plan on the quests still written for
Babel): acceptance is of the path rule
([epic](/quest/m2/cluster-routes/README.md) Plan item 27), on the
sessions [the switch](/quest/m2/cluster-routes/switch.md) leaves.
The seqno request fixtures are replaced below, since the rule has no seqno
and no request.

- Port the forwarding fixtures (`Forward`, `Aggregate`, pool ties, and the
  sweeps over them) to the implementation, then delete the model.
- The earlier simulator persisted seqnos across restart, and the path rule
  has none. Re-run with a fresh routing identity per restart and the
  stale-state rules; do not carry the old assumption into acceptance.
- Retire [moq-dev/moq#5375](https://github.com/moq-dev/moq/pull/5375)'s
  quarantine, [moq-dev/moq#5391](https://github.com/moq-dev/moq/pull/5391)'s
  dedup, and the 300 ms hold only once the fixtures below demonstrate their
  replacement (see the epic's hold-down note).
- Add a session-level fixture for routes scoped to interested origins
  (Plan item 15 of the epic, implemented on the router): a client watching
  one prefix holds routes only to the origins whose claims meet it, a
  request for a path outside its interest still resolves through the relay
  (Node ID 0), and an origin leaving the interest ends its route after the
  batch withdrawing its last claim there, closed by its floor: one that
  certifies the withdrawal where the relay certifies the request, and one
  certifying nothing where it knows only part of it.

The counterexamples from #4349 (closed; this epic still owns the fix) are the `residual_*` tests in
`rs/moq-net/tests/integration/mesh_withdraw.rs`. They pin today's hunting on lite-06:
a stale path outlasting the 300 ms hold, a revival the quarantine misses,
and 179 hunting relays (3 reviving) over 200 seeded meshes. Turn each into a
single retraction everywhere, and keep `cross_node_close_retracts_once`
passing. On `main` an epochless switch to a stale path is a Restart, which a
lite-06 client sees as a retraction and an announcement; the release sent an
in-place update.

Publishing toggles too, decided 2026-10-13: a longer path arrives first and
the better route then replaces it. `late_better_path_updates_in_place` pins
the lite-07 mesh, where the move keeps the instance and a lite-06 or lite-07
client sees an update. `residual_late_better_path_restarts_on_lite06` pins the
lite-06 mesh, where each relay mints its own instance and the client sees
`Start, End, Start` while the broadcast stays live; turn it into an update.

Tests, time mocked: the handoff fixtures and sweeps, already on the router;
[moq-dev/moq#4644](https://github.com/moq-dev/moq/pull/4644)'s live-graph withdrawal and failover tests (the PR closed and
its branch is gone; fetch `refs/pull/4644/head`) as acceptance, run on the wip version; a
link flap with many broadcasts behind one origin costs one ROUTE change per
link; a ring of three where a stale neighbour cannot revive an ended
broadcast; a two-uplink mesh that never carries CDN routes back to the CDN;
an island reached through the CDN after a partition; an unknown Route ID
refused; a five-node ring whose preferred origin link fails moves to its
longer backup, at once where the backup's path avoids the far end of the
closed link and once the doubt lifts where it runs through it, while that
backup never loses its session; a relay with several neighbours that dies
is named in one withdrawal per session, and no relay moves onto a route
through it before the doubt lifts; two relays whose only routes run through
each other both let go, neither holding for the other; and a relay that
holds lets go once the neighbours behind it have answered, including one
whose session closes without an answer. Failover is measured
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

Mounts get the same continuity checks (`rs/moq-net/tests/integration/mount.rs`):
a claim whose predecessor covers a mount's target arrives beneath the mount
point exactly as it does unmounted, on lite-06 and lite-07. This replaces
the deleted mount-takeover quest, whose `Takeover` event
[moq-dev/moq#8063](https://github.com/moq-dev/moq/pull/8063) removes in
favor of the claim's instance; if #8063 closes unmerged, restore that quest.

Benchmark origins, broadcasts per origin, neighbors, active tracks/fronts,
and announce cursors independently. A route update must not expand back into
per-broadcast table rewrites or poll unrelated fronts. Measure messages, bytes,
wakes/select calls, CPU, replay work, and retained state. One broadcast per
origin is an explicit case. Coordinate with Route wakes and Announce replay;
benchmark smoke alone does not enforce complexity, so add operation-count
regressions. Bound held routes, doubts, and remembered dead hops under
same-clock churn.

## Required

- [lite-07 sessions run the path router](/quest/m2/cluster-routes/switch.md) - the sessions and the route rule these fixtures accept

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the contract and its fixtures
