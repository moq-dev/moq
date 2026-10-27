# [XS] Seeded BBR3 in tests

## Goal

Every moq-congestion test that builds `Bbr3Config::default()` without a seed
replays the same randomness on every run, as it did before
[moq-dev/moq#6602](https://github.com/moq-dev/moq/pull/6602).

## Plan

Implemented on this branch: `Bbr3::new` falls back to a fixed seed under
`cfg(test)`. Explicit seeds retain their precedence, and production retains
OS randomness. Two default controllers now replay their probe schedules;
before the fix their first waits were 2.193 s and 2.292 s. The selected suite
passes 96 tests, with one existing skip, including the explicit-seed test.

Remaining: finish the required scoped gates once the unrelated base Rust
failures are fixed, then remove this quest.

- `just fix` reaches `rs/moq-mux/benches/catalog.rs:226`, which still refers
  to the `hang::catalog::MAX_RENDITIONS` constant removed by
  [moq-dev/moq#6827](https://github.com/moq-dev/moq/pull/6827). The benchmark
  arrived in [moq-dev/moq#6784](https://github.com/moq-dev/moq/pull/6784).
- `just rs fix -p moq-congestion` passes. Its unrelated CARLA formatting
  output was restored; the base's `demo/carla/bridge` files still fail the
  workspace-wide format check. No open fix was found before this checkpoint.
- Run `just ci` on the clean committed head; this checkpoint leaves full
  validation pending rather than relaxing the gate or mixing base repairs.
