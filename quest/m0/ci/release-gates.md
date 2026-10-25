# [XS] Require the release gate

## Goal

`release` carries `.github/workflows/release-gate.yml`, backported now as
release-line maintenance rather than waiting for the next cut (decided
2026-10-22, maintainer). Check with `git show origin/release:.github/workflows/release-gate.yml`.

Then the maintainer adds `Release gate` from the GitHub Actions integration
(15368) to release ruleset 24392632, beside `Check`, so PRs into `release`
cannot merge until the platform and optional workflows pass. Main ruleset
2420853 already requires only `Check`.

## Plan

Decided 2026-10-19: expensive and optional checks are required on `release`
and run only nightly on `main`.

- `release-gate.yml` calls Platform, Features, and every path-selected optional
  workflow, and its `Release gate` job fails when any of them fails or is
  cancelled. Skipped workflows pass it, so one context replaces a list of job
  names that each rename would break. `sh/gh/queue.test.ts` holds the policy.
- A release PR runs the workflows merged with `release`, so a PR opened before
  the cut never reports `Release gate` and would block once it is required.
  Add the context right after the cut merges, then edit the title of each open
  release PR; `edited` reruns the gate.
- Before then, release PRs still run the gate; it just does not block.
- Measured 2026-10-14 to 10-16, for sizing the release PR wait: of 100 queue
  macOS jobs, runner wait was p50 0.2, p90 33 and max 83 minutes. Nightly
  and releases share the five-job macOS cap.
