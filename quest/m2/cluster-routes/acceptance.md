# [M] Route layer acceptance

## Goal

The route layer is proven on the implementation, and the mitigations its
fixtures replace are retired: [moq-dev/moq#5375](https://github.com/moq-dev/moq/pull/5375)'s
retraction quarantine, [moq-dev/moq#5391](https://github.com/moq-dev/moq/pull/5391)'s
same-clock queue dedup, and the 300 ms update hold, all marked `lite06_churn` in
`rs/moq-net/src/model/origin.rs`.

## Plan

Before the lite-07 freeze, exercise a scoped subscriber whose permitted, disjoint prefixes
cannot fit one ANNOUNCE_REQUEST and cannot be widened within the grant. In
`lite/subscriber/routed.rs`, `asks` can discard a prefix while `release` treats an
empty set of matching replies as synchronized. Publish inside the last permitted region
and verify either complete discovery or a surfaced inability to answer. An empty,
synchronized snapshot for an unasked region is incorrect. The audit traced these branches
in [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987); confirm the behavior on real sessions and fix it if reproduced.

Decided 2026-11-14 (maintainer, /quest-complete on
[moq-dev/moq#8768](https://github.com/moq-dev/moq/pull/8768)): ✅ stays in
m2 and gates the cut, per lite07-finalize's 2026-10-13 rule that routes'
tier and partition tests run before the cut (rejected: moving it to m3 and
fixing late routing flaws in lite-08). A flaw found here is fixed on
lite-07-wip before it freezes.

The fixtures landed in [moq-dev/moq#8832](https://github.com/moq-dev/moq/pull/8832),
and the reference model is deleted. On real sessions with time mocked: the
topology and prior-art fixtures (`tests/integration/route_acceptance.rs`),
failover liveness and the live-graph tests of
[moq-dev/moq#4644](https://github.com/moq-dev/moq/pull/4644) (`route_failover.rs`),
the tiered cluster, partition, and scoped-interest checks (`route_cluster.rs`),
and mount continuity (`mount.rs`).
[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) put lite-07 on
`route::relay::Router` and rewrote their assertions for it; the old
router's forwarding and operation-count fixtures went with it, and
`route/tests/multicast*.rs` covers the new one. The route fan-out quest
those pinned was deleted
with the old rule, its one surviving cost moved to
[route wakes](/quest/m4/route-wakes.md).

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): the
switch makes `origin::Config::update_hold` crate-private, since nothing
outside moq-net sets it, and puts whatever of the three mitigations survives
here under one named lite-06 marker. So retiring them below changes no
public API, and one that a lite-06 mesh keeps is already marked temporary.
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053) did both: the
three are marked by the `lite06_churn` module, and still run on every
version, so gating any that a lite-06 mesh keeps to the lite-06 session
path is part of retiring them below.

What remains:

- Retire the quarantine, the dedup, and the hold, now that the fixtures
  run on the route layer that replaces them. The quarantine
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

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the contract and its fixtures
