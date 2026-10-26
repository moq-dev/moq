# [XS] Require the release gate

## Goal

Waiting on the maintainer. [moq-dev/moq#6733](https://github.com/moq-dev/moq/pull/6733)
backports the gate onto `release`'s own workflow set. Once it merges
(check with `git show origin/release:.github/workflows/release-gate.yml`), the
maintainer adds `Release gate` from the GitHub Actions integration (15368) to
release ruleset 24392632, beside `Check`, so PRs into `release` cannot merge
until the platform and optional workflows pass. Main ruleset 2420853 already
requires only `Check`. Delete this quest once the ruleset requires it.

## Plan

Decided 2026-10-19: expensive and optional checks are required on `release`
and run only nightly on `main`.

- `release-gate.yml` calls every path-selected optional workflow, and its
  `Release gate` job fails when any of them fails or is cancelled. Skipped
  workflows pass it, so one context replaces a list of job names that each
  rename would break. On `main`, `sh/gh/queue.test.ts` holds the policy.
- A release PR runs the workflows merged with `release`, so a PR opened before
  the backport merges never reports `Release gate` and would block once it is
  required. Add the context right after it merges, then edit the title of each
  open release PR; `edited` reruns the gate.
- The next cut conflicts on the backported workflow files; take `main`'s side.
- Measured 2026-10-14 to 10-16, for sizing the release PR wait: of 100 queue
  macOS jobs, runner wait was p50 0.2, p90 33 and max 83 minutes. Nightly
  and releases share the five-job macOS cap.
