# [S] moq-net's async helpers sit behind a feature

## Goal

moq-net's async helper methods sit behind an `async` cargo feature, and a CI
lane builds and tests the crate without it, so rs2ts reads the no-runtime
crate that [Generated lite](/quest/m1/rs2ts/lite.md) translates.

## Plan

- The feature is on by default, so Rust callers see no change. JS
  reimplements the helpers with Promises over the poll API.
- Generated lite needs the lite session and the model without the feature,
  not IETF. Until the [Sans-IO IETF session](/quest/m1/rs2ts/sans-io/ietf.md)
  lands, the IETF session can sit behind the feature too; that quest then
  moves it out.
- The lane runs at least the tests that do not exercise the helpers; tests
  that do stay behind the feature.

Public API: moq-net's async helpers move behind a default feature, so a
`default-features = false` caller loses them; lands on `dev` with the line.
Wire: none.

## Required

- [Sans-IO lite session](/quest/m1/rs2ts/sans-io/lite.md) - the session builds without a runtime
