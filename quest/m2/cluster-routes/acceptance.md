# [M] Route layer acceptance

## Goal

The route layer is proven on the implementation, and the mitigations its
fixtures replace are retired: [moq-dev/moq#5375](https://github.com/moq-dev/moq/pull/5375)'s
retraction quarantine, [moq-dev/moq#5391](https://github.com/moq-dev/moq/pull/5391)'s
same-clock queue dedup, and the 300 ms update hold (`origin::Config::update_hold`).

## Plan

Decided 2026-11-14 (maintainer, /quest-complete on
[moq-dev/moq#8768](https://github.com/moq-dev/moq/pull/8768)): ✅ stays in
m2 and gates the cut, per lite07-finalize's 2026-10-13 rule that routes'
tier and partition tests run before the cut (rejected: moving it to m3 and
fixing late routing flaws in lite-08). A flaw found here is fixed on
lite-07-wip before it freezes.

The fixtures landed in [moq-dev/moq#8832](https://github.com/moq-dev/moq/pull/8832),
and the reference model is deleted: forwarding on the router
(`rs/moq-net/src/route/tests/forward.rs`), and on real sessions with time mocked
the topology and prior-art fixtures (`tests/integration/route_acceptance.rs`),
failover liveness and the live-graph tests of
[moq-dev/moq#4644](https://github.com/moq-dev/moq/pull/4644) (`route_failover.rs`),
the tiered cluster, partition, and scoped-interest checks (`route_cluster.rs`),
mount continuity (`mount.rs`), and operation-count regressions
(`route/tests/cost.rs`, `route/node/tests.rs`). They found the flaws the
quests under Required fix; the tests that pin each flaw say so in their docs.

What remains:

- Retire the quarantine, the dedup, and the hold once the Required quests
  turn their pinned fixtures into the contract's behaviour. The quarantine
  is what turns the flaws into a 2 s gap: a relay that shows a held claim
  offline announces it again only after `RETRACT_QUARANTINE`. The hold and
  the quarantine sit on the announce cursor, which every version shares, so
  check what a lite-06 mesh loses without them
  (`mesh_withdraw::cross_node_close_without_hold_reannounces` is its
  positive control) before deleting them; if a lite-06 mesh still needs
  them, retire them for lite-07 routes only.
- `mesh_withdraw::residual_late_better_path_restarts_on_lite06` pins a
  lite-06 mesh where each relay mints its own instance, so a client sees
  `Start, End, Start` while the broadcast stays live. Turn it into an update,
  or record why a lite-06 mesh keeps it.
- A request naming Node ID 0 on a lite-07 session: Rust clients always name
  a node, so only publisher unit tests cover a relay answering 0. Cover it
  where `js/net` sends it, in `just test interop`.

## Required

- [Held after close](/quest/m2/cluster-routes/held-after-close.md) - a relay whose own session closed shows its held claims unavailable, not offline
- [Doubt holds](/quest/m2/cluster-routes/doubt-holds.md) - a relay keeps holding while doubt alone keeps it off its routes
- [Route fan-out](/quest/m2/cluster-routes/route-fanout.md) - a route change costs only the streams and broadcasts it touches

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the contract and its fixtures
- [Doubt and keep-alive](/quest/m2/cluster-routes/doubt-keepalive.md) - whether the doubt timer follows the keep-alive
