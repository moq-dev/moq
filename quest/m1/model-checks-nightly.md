# [M] The TLA+ models run nightly and reviews resync them

## Goal

Every TLA+ model in the tree runs in Nightly: each passing config must pass,
and each open-finding config must still fail, so a fix that should promote
its rule is noticed. A PR that changes code a model covers gets that model
rerun and resynced in review, and renaming or deleting a function a model
covers fails `just check` until its skill names the new one, so no model
keeps passing on code that no longer exists.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan), after [moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)
added the models (`rs/moq-net/verify/*`, `js/*/verify/`) and their `verify-*`
skills.

- Nightly runs every config through the `tla` recipes. TLC runs take 20 s
  to 2 min on 32 cores, so they stay out of the merge gate. An open-finding
  config is one its skill lists under "Open finding"; the job expects TLC's
  invariant-violation exit for it and fails on a pass.
- The JS flow models (`js/*/verify/`) check every rule in one config, so
  each breaks on its open findings. Split them as the Rust models are: a
  passing config plus one per open finding. Publish's `LiveMeansWatchable`
  break is accepted (2026-11-10, a badge that leads the announce by
  milliseconds), so it leaves the passing config.
- ✅ Drift is flagged by a review rule (rejected: a CI check that fails a PR
  touching covered files without touching the model, which taxes every
  refactor and comment edit; and both). `REVIEW.md` tells the reviewer to run
  the matching `verify-*` skill when a PR changes a file in that skill's
  Coverage table, and the PR updates the model and its sync commit.
- The periodic resync for changes that skipped review is a check-first item
  in the [test coverage audit](/quest/m3/audit-test-coverage.md).
- ✅ `just check` also fails when a function a `verify-*` skill's Coverage
  table names no longer exists in the file the row names (decided 2026-11-10,
  maintainer, a later /quest-plan the same day; rejected: the review rule
  alone). Unlike the touched-file check rejected above, it fires only when
  modelled code is renamed or deleted, which is when the table's mapping is
  already wrong. Give the tables one parseable form (a file, then its
  symbols) and test the check on a renamed symbol.

Public API: none. Wire: none.
