# [S] moq-net's async helpers sit behind a feature

## Goal

moq-net's async helper methods sit behind an `async` cargo feature, and a CI
lane builds and tests the crate without it. Without the feature, both
sessions (lite and IETF) are compiled out entirely, leaving the model's
explicit-time interface for Controlled time.

## Plan

- The feature is on by default, so Rust callers see no change.
- Gate the sessions whole rather than requiring a sans-IO rewrite. This
  stays in m1 because [Controlled time](/quest/m1/time/net.md) requires it,
  independently of the browser backend selection.
- The model's `run_front`/`serve_front` (`model/origin.rs`) are core driver
  logic, not helpers, so they stay outside the feature; they need no runtime
  to build. Production WASM may retain async Rust. Rewriting these drivers
  or reimplementing helpers with JS Promises is not part of this quest.
- The lane runs at least the tests that do not exercise the helpers; tests
  that do stay behind the feature.

Public API: moq-net's async helpers and sessions move behind a default
feature, so a `default-features = false` caller loses them. Wire: none.
