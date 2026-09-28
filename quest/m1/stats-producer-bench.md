# [S] Stats producer fan-out benchmark

## Goal

A `moq-stats` benchmark measures what one relay pays per stats tick to drain
its registry and encode the traffic tracks, swept over held paths and tiers,
so a cost that grows with the whole table instead of the paths that changed
shows up as a slope. It runs at least nightly.

## Plan

- [#4299](https://github.com/moq-dev/moq/pull/4299) keeps an idle path in
  every frame while the registry holds its counters, adding about 430 B per
  idle path to each plain frame (maintainer's note on the PR). Nothing
  measures what that, or the per-tick drain, costs as held paths grow.
  `rs/moq-stats/benches/decode.rs` covers only the reader.
- Sweep held paths (for example 100 to 50k) against tiers, and the share of
  paths that are idle versus changed this tick. Report time and allocations
  per tick, and plain and compressed bytes per frame, the way `decode.rs`
  reports its table. Include the point where a plain frame nears its size
  cap, since a frame that is too large leaves stale counters behind.
- Drive the real producer path (`process_slot`, the snapshot encoders) over
  a synthetic `Registry`, not a re-implementation of it. Exposing a bench
  hook is fine if it stays out of the public API.
- `decode.rs` is not in CI either. Run both targets once in the nightly
  benchmark smoke, as `moq-net`'s are.

Public API: none. Wire: none.

## Related

- [Binary delta stats flavor](/quest/m2/stats-delta.md) - its gate needs the encode baseline this measures
- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - compares these targets across PRs once it lands
