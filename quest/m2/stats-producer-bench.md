# [S] Benchmark the moq-stats producer

## Goal

A Criterion target measures what one relay pays per stats tick to drain its
registry and encode the traffic tracks, swept over held paths and tiers, and
runs in the nightly benchmark smoke. It is the moq-json snapshot encoder
profile [Binary delta stats](/quest/m2/stats-delta.md) gates on.

## Plan

Split out of [Bench coverage](/quest/m2/bench-coverage.md) in the 2026-10-06
audit, so the stats-delta gate waits on this benchmark alone rather than on
four unrelated ones.

- #4299 keeps an idle path in every frame while the registry holds its
  counters (about 430 B per idle path per plain frame), and nothing measures
  that or the per-tick drain as held paths grow. Sweep held paths (for
  example 100 to 50k) against tiers and the share of idle versus changed
  paths.
- Report time and allocations per tick, and plain and compressed bytes per
  frame the way `rs/moq-stats/benches/decode.rs` reports its table,
  including the point where a plain frame nears its size cap.
- Drive the real producer path (`process_slot`, the snapshot encoders) over
  a synthetic `Registry`; a bench hook is fine if it stays out of the public
  API.
- `decode.rs` is not in CI either; run both targets in the nightly benchmark
  smoke, as `moq-net`'s are.

Name the target after what it measures, so the CI comment reads without
opening the file.

Public API: none. Wire: none.

## Related

- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - tracks this target on PRs and nightly
