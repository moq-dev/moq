# [S] Relay memory benchmark

## Goal

A committed benchmark states what a relay's memory costs per announced
broadcast, per extra route, and per peer session, so the rest of
[Cluster routing](/quest/m1/cluster-routing/README.md) has a before figure and
reports its after figure against the same code. Chat-shaped traffic (one
broadcast per channel or per chatter) and Wildcard's "workers times
broadcasts" argument both depend on the answer.

## Plan

Every published figure is stale. The old baseline was 8.8 KB per announced
broadcast plus 4.3 KB per extra route on `adad52b`, measured with two
throwaway `moq-net` examples driving an origin under a counting allocator and
reading `/proc/self/statm`. Since then
[moq#2989](https://github.com/moq-dev/moq/pull/2989) cut `kio`'s inline waiter
slots from 32 to 4, and [moq#3225](https://github.com/moq-dev/moq/pull/3225)
made a standby route a table entry rather than an object graph. Neither
example was committed, since they needed `#[doc(hidden)]` size probes on
private types.

- Measure through the public API with a counting allocator (as
  `rs/moq-net/benches/session.rs` already does for groups), not private size
  probes, so the benchmark survives the redesign.
- Sweep both axes: broadcasts announced, and peers each one is routed through,
  so a cost that grows with the table rather than the touched path shows as a
  slope.
- Keep two costs apart: the route table and per-announcement state
  (`RouteEntry`, `ServeState` in `rs/moq-net/src/model/origin.rs`) scale with
  announcements and routes, while the served-content cache a `ServeState`
  materializes scales with demand. Per-peer bookkeeping is the lite
  publisher's `live` map and `AnnounceEncoder` entries, the lite subscriber's
  `Announced.routes`, and the IETF `watched` and `held`.
- Run it in the nightly benchmark job
  ([Benchmark regressions in CI](/quest/m1/bench-ci.md)) or its own nightly
  step if that has not landed.
- Record the before figures in the PR and in the questline README, and derive
  the shed threshold on a degree-5, 1 GB node from them. The implementation
  child that replaces per-peer routes reports the after figures.

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - cites the stale figure
- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - where the benchmark runs nightly
