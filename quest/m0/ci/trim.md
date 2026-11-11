# [M] CI lanes that fail or repeat the queue are cut

## Goal

Every workflow either produces a signal someone reads or is gone. Today
`cache.yml` fails every run, `cache-refresh.yml` has never passed,
`mutants.yml` has never produced a result, and three workflows re-run on
`main` what the merge queue already ran.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan from the deletion audit).
Decided: ✅ CI trims (rejected: leave CI as the 2026-11-01 workflow audit
kept it). Joins m0 because `cache.yml` is broken now (rule: broken CI is m0).

- `cache.yml`: every push to `main` fails in the warm jobs with
  `does not contain a 'flake.nix'` (the `roots` recipe runs outside the
  checkout), including the macOS job. Fix it and add it to `alert.yml`, or
  delete it if the R2 and nix.moq.dev caches already cover what it warms;
  reproduce first and say which in the PR. Check
  [Cachix retire](/quest/m0/ci/cachix-retire.md), which also edits it.
- Delete `cache-refresh.yml`. It guards against weeks without a merge, and
  `main` lands more than ten PRs a day.
- Delete `mutants.yml`. Its one run failed all 24 shards creating
  `target/mutants`, and `test/drill/sensitivity.sh` already runs hand-written
  mutations nightly. This PR deleted its quest.
- `codspeed.yml` moves from every Rust push on `main` to the nightly
  schedule. It gates nothing, and 62 of its last 100 runs were cancelled by
  the next push. The 2026-10-28 decision to keep CodSpeed stands; only its
  trigger changes.
- `check.yml` stops running for PRs into `main` and keeps `release`
  admission; the queue runs the same `just ci`.
- `quest.yml` drops its `main` push trigger and keeps `release` and
  `quest/**`; `main` lands only through the queue, which runs the quest
  checks.

Update any doc naming the removed workflows.

Public API: none. Wire: none.
