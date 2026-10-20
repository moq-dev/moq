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
  integration (15368) and nothing else. Keep that until
  [one CI pass](/quest/m0/ci/one-pass.md) lands, then replace both with the
  single job's name right after it merges, before enqueuing anything else.
  An older open PR reports the new name after its next push or edit.
- Release ruleset 24392632 has no merge queue and requires `Check` and
  `Test`. Add `Platform scope`, `macOS`, `Windows`, and the workflows
  [cloud CI only in the queue](/quest/m0/ci/queue-only.md) moves off main
  PRs. Only require a context that every release PR reports: a workflow with
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
