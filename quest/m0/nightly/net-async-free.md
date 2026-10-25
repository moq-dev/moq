# [S] moq-net builds without async

## Goal

The `moq-net --no-default-features` lane compiles and lints moq-net without
the `async` feature, so async-only code can't leak into the sync build.

## Plan

- Today the lane's `cargo clippy --all-targets` and `cargo nextest run --tests`
  steps (`sh/rs/net-features.sh`) build dev-dependencies, which pull `async`
  back in: moq-net dev-depends on itself with `fuzz`, which implies `async`,
  and on moq-json, whose `moq-net = { workspace = true }` may enable the
  default features (confirm with `cargo tree -e features`).
- Replace the `--all-targets` clippy with `--lib`, which skips
  dev-dependencies. For tests, move every test target that needs those
  dev-dependencies into its own test crate, so the lane's nextest builds
  moq-net without `async`.
