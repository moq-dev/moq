# [S] moq-net's async helpers sit behind a feature

## Goal

moq-net's async helper methods sit behind an `async` cargo feature, and a CI
lane builds and tests the crate without it. Without the feature, both
sessions (lite and IETF) are compiled out entirely, so the remaining crate is
the no-runtime model that [Generated model](/quest/m2/rs2ts-model.md)
translates.

## Plan

- The feature is on by default, so Rust callers see no change. JS
  reimplements the helpers with Promises over the poll API.
- Decided 2026-10-11: gate the sessions whole rather than wait on their
  sans-IO rewrites, which now wait on the generated model. The
  [sans-IO lite](/quest/m2/rs2ts-sans-io-lite.md) and
  [sans-IO IETF](/quest/m2/rs2ts-sans-io-ietf.md) quests each move their
  session out from behind the feature. That leaves this ready, and it stays
  in m1 because [Controlled time](/quest/m1/time/net.md) requires it.
- The model's `run_front`/`serve_front` (`model/origin.rs`) are core driver
  logic, not helpers, so they stay outside the feature; they need no runtime
  to build. rs2ts does not translate async, so
  [Generated model](/quest/m2/rs2ts-model.md) rewrites them as a poll
  machine.
- The lane runs at least the tests that do not exercise the helpers; tests
  that do stay behind the feature.

Public API: moq-net's async helpers and sessions move behind a default
feature, so a `default-features = false` caller loses them. Wire: none.
