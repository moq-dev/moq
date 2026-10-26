# [S] Replay full-run failures against CI's selected gate

## Goal

Find the dependency edges `just ci`'s selection misses. Replay each change
whose full run (Nightly or `just gate`) failed where its selected queue run
passed, and fix every missed edge with a test in `sh/changed.test.py`,
`sh/dispatch-query.test.py`, `sh/rs/select.test.py`, or their neighbors.

## Plan

- [The CI selection report](/.github/ci-selection-audit.md) covers widening
  frequency for September 26 through October 25 and its per-attempt evidence.
  Extend it with the replay, rather than starting a second report.
- Replay against the selection rules each change's queue run used, read from
  that head's `sh/dispatch.sh` and `sh/rs/select.sh`.
- Separate actual missed edges from failures a narrower selection cannot cause,
  such as checkout-history problems and feature unification already owned by
  [moq-dev/moq#6830](https://github.com/moq-dev/moq/pull/6830) and
  [moq-dev/moq#6851](https://github.com/moq-dev/moq/pull/6851).
- Decided with the maintainer on 2026-10-26: the report's incidence counts and
  archived elapsed durations stand in for runner CPU time, so no CPU
  measurement window is needed. Cargo.lock stays a whole-workspace trigger; a
  lockfile-diff selector would narrow at most 75 attempts a month.
- kixelated/rust2's
  [MIR test selection](https://github.com/kixelated/rust2/blob/main/quest/m3/test-select.md)
  measures a finer selector against this one.

Public API: none.
