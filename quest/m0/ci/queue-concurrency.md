# [M] Build queue entries concurrently

## Goal

The merge queue builds two or more entries at once on self-hosted capacity, so
merged PRs per hour rise measurably over the 2026-10-23 baseline while every
entry still runs the same gate.

## Plan

Decided with the maintainer on 2026-10-23: structural queue changes come before
faster toolchains.

Measured 2026-10-23 over 32 merge-queue runs: `max_entries_to_build: 1` with one
`moq-queue` runner makes the queue fully serial, about 7 merged PRs per hour,
with a median entry of 135 s (p90 516 s) and 3 to 12 s of runner wait.

- Add queue runner capacity (another instance or container) and raise
  `max_entries_to_build` to match. The host also runs the `moq-pro` runner
  under the same `ci.slice` quota and a desktop; split CPU and memory so
  concurrent entries do not slow each other past the gain.
- Concurrent entries share the local mbx store; confirm that is safe, or give
  each its own.
- Measure merged PRs per hour and per-entry wall time before and after.
- Queue entries skip `quest gate`: `sh/local.sh` execs directly under GitHub
  Actions, so the runner quota, not admission, has to keep entries apart.

Public API: none. Wire: none.
