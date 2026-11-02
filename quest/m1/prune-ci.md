# [M] Delete the CI jobs that don't earn their cost

## Goal

Every CI workflow and job, on PRs, the queue, Nightly, and schedules, is worth
its runner time. One that doesn't is deleted. A costly one that earns its
keep is made cheaper or moved off the merge path. Individual tests belong to
[Delete the tests that don't earn their cost](/quest/m1/prune-tests.md).

## Plan

This is a one-off, off-cycle run of the [CI workflow audit](/quest/m2/audit-ci.md),
ahead of the cut. The recurring audit keeps its schedule, and this quest
creates no successor. Its PR rewrites that audit's per-workflow table and
baselines with what this run learned.

Decided 2026-11-01 (maintainer):

- Default to deleting. A job goes when its runner time outweighs the
  regressions it alone catches, or when nobody sees its result.
- Delete inline in this quest's PR, one commit per workflow. The PR body
  lists each deletion with its runner minutes and why it wasn't worth it.
  Dropping a release target or a published artifact is a follow-up through
  `/quest-plan`.

Use the m2 audit's signal, visibility, and cost verdicts, but treat any job
that hasn't caught anything since the m1 run (2026-10-20) as a deletion
candidate. Only keep it if you can say what it would uniquely catch.
