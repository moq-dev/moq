# [XS] Require the release gates

## Goal

PRs into `release` (cuts and backports) cannot merge until the platform and
optional workflows pass, and main's merge queue requires only the Linux gate.
A maintainer edits the rulesets; the implementation PRs make no ruleset
changes.

## Plan

Decided 2026-10-19: expensive and optional checks are required on `release`
and run only nightly on `main`. This supersedes the 2026-10-14 plan to
require `Platform scope`, `macOS` and `Windows` in main's merge queue, which
never got activated.

- Main ruleset 2420853 requires `Check` and `Test` from the GitHub Actions
  integration (15368) and nothing else. [One CI pass](/quest/m0/ci/one-pass.md)
  folds them into one job that keeps the name `Check`. Remove `Test` from the
  ruleset just before that PR is enqueued. Removing a context only loosens
  the gate, and groups built earlier still report both, so nothing waits on a
  context the workflow stops reporting.
- Release ruleset 24392632 has no merge queue and requires `Check` and
  `Test`. In the same edit as the cut that carries the one-pass workflow to
  `release`, remove `Test` there too, and add `Platform scope`, `macOS`,
  `Windows`, and the optional workflows that trigger only on release PRs
  (`sh/gh/queue.test.ts` lists the policy). Only require a context that every release PR reports: a workflow with
  path filters never reports on an unrelated diff and would block forever,
  so it needs an always-run wrapper job first (the `platform.yml` pattern).
- Require `Platform scope` too: a failed selector skips its OS dependents,
  and skipped jobs alone satisfy required checks.
- A release PR runs the workflows merged with `release`, so wait until the
  next cut carries these workflows there. Before that, backports would wait
  forever on contexts the old workflows never report.
- Measured 2026-10-14 to 10-16, for sizing the release PR wait: of 100 queue
  macOS jobs, runner wait was p50 0.2, p90 33 and max 83 minutes. Nightly
  and releases share the five-job macOS cap.

## Required

- [One CI pass](/quest/m0/ci/one-pass.md) - settles main's required job name
