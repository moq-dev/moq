# [S] Back off an unreachable probe target

## Goal

A session whose probes keep failing to raise the estimate toward its target
backs the target off, instead of spending about 5% of its media on duplicate
probe bytes indefinitely.

## Plan

- Measured in [moq-dev/moq#6635](https://github.com/moq-dev/moq/pull/6635):
  4.6% redundant bytes for a 100 Mbit/s target on a 3 Mbit/s path.
  `rs/moq-quic/src/connection/bandwidth_probe.rs` keeps no state about failed
  rises.
- Back off exponentially after a number of ProbeBW_UP cycles with no estimate
  rise, and reset when the target changes. Prove it on a bufferbloat path.

## Related

- [App-limited BBR3 estimate ages](/quest/m1/congestion/app-limited-estimate.md) - the estimate the probe compares against
