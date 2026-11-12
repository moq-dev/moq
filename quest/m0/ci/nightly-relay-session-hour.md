# [M] Nightly's relay session benchmarks fit their hour

## Goal

`Nightly (Relay session benchmarks)` (`just gh nightly rust-relay`, a paired
group that measures this commit and the previous Nightly's) finishes inside
its 60-minute `timeout-minutes` on a scheduled run and keeps its paired
regression gate.

## Plan

The job does not hang and the sweep did not grow: one cell's teardown went
super-linear. In
[the 11-10 run](https://github.com/moq-dev/moq/actions/runs/42842827718/job/129922278379)
each pass (this commit, then the base) spent about 22 minutes between the
`relay_session_delivery_prefixes/moq-lite-07-wip/broadcasts=1025/follow=1024`
cell and the next one; on
[11-07](https://github.com/moq-dev/moq/actions/runs/42377867523) that gap was
under a second. A local run of
`cargo bench -p moq-relay --features test-support --bench session -- --quick relay_session_delivery_prefixes/`
reproduces it: one thread spins at 100% for over 10 minutes after the lite-07
`follow=1024` cell, while the next cell alone finishes in 2 seconds.

The spinning thread is the cell's `retire`, dropping the viewer's lite-07
session driver. Each of its 1024 `AnnouncePrefix` streams drops a
`route::node::Stream` (`rs/moq-net/src/route/node.rs` `Drop`), which calls
`disconnect`, then `update`, `recompute`, `prune` (`route/mod.rs`), and
`View::restrict_all` over every region still certified
(`route/view.rs`). `restrict_all` merges one restriction per region, and each
`merge_claims` costs claims x floors in `as_of` plus floors squared in its
stale check and floor `retain`. With about 1024 regions, claims, and floors,
that is about 1e9 steps per closed prefix and cubic or worse over the session.
A relay pays this whenever a lite-07 peer holding many announce prefixes
leaves, so it is a relay CPU cost a client controls, not only a benchmark
problem.

Make the route layer's teardown of a neighbor's prefixes cost about linear in
what it held: for example, let `restrict_all` build one view from all regions
in a single pass instead of merging per region, index floors so `as_of` and
the floor `retain` stop scanning every floor, and disconnect a session's
prefixes as one batch. Keep `/verify-route` passing. Then confirm the Relay
session group finishes inside the hour (the 11-06 and 11-07 sweeps took
about 25 minutes) without dropping the `follow=1024` cell, which is the case
that exposed this. Add a route-layer benchmark sweeping held prefixes on
both axes if the session cell does not isolate the teardown cost.

## Related

- [Rust benchmark tests fit their hour](/quest/m0/ci/nightly-rust-test-hour.md) - the same timeout, caused by builds
