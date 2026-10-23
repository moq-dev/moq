# [S] Fail early on repository tooling errors

## Goal

Every merge-queue Check runs the cheap repository checks and tooling tests
before expensive builds, so a tooling regression releases the runner quickly.
Keep the same selected coverage and run each check once.

## Plan

Decided 2026-10-23: prioritize earlier failure feedback. This changes the cost
of failing runs; it does not promise a faster successful gate.

- In [queue run 39996587979](https://github.com/moq-dev/moq/actions/runs/39996587979),
  the tooling tests failed after 18 seconds, but only after about 16 minutes
  of preceding work. The selector fix landed in
  [#6555](https://github.com/moq-dev/moq/pull/6555).
- The full `just gh test` suite passed locally in 36 seconds with
  `GITHUB_ACTIONS=true` on `b2899cc08` plus the benchmark-coverage correction
  already included in [#6480](https://github.com/moq-dev/moq/pull/6480).
  This is a diagnostic sample, not a controlled runner benchmark.
- Start from `sh/dispatch.sh` and `.github/justfile`. Put cheap repository
  lints and tooling tests ahead of builds while preserving upfront tool
  validation, path selection, local entrypoints, and timing reports. Avoid
  duplicating the tooling tests introduced by the prerequisite.
- Verify that an injected tooling failure prevents expensive work from
  starting, and that successful dispatch still runs every selected check
  exactly once. Compare time to failure and successful gate duration on the
  same runner and cache state; record both in the implementation PR.

## Required

- [Tooling tests gate every merge](/quest/m1/gh-test-gate.md) - establish the always-selected tooling test gate before changing its order
