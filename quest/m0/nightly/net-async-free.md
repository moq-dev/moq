# [S] moq-net builds without async

## Goal

The `moq-net --no-default-features` lane compiles and lints moq-net without
the `async` feature, so async-only code can't leak into the sync build.

## Plan

- Today the lane's clippy and nextest steps (`sh/rs/net-features.sh`) pull
  `async` back in: moq-net dev-depends on itself with `fuzz`, which implies
  `async`, and on moq-json, which enables moq-net's default features.
- Add `cargo clippy -p moq-net --no-default-features --lib`, which skips
  dev-dependencies, and move the fuzz-feature integration tests to their own
  test crate if the lane still needs to run tests.
