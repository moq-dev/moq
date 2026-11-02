# [M] Delete the tests that don't earn their cost

## Goal

Every test in the merge gate and in nightly is worth its cost. A test whose
wall time, CPU, or flakiness outweighs the regression it alone catches is
deleted. A costly test that earns its keep is made cheaper: fast at its
source, or moved to nightly. CI workflows and jobs belong to
[CI workflow audit](/quest/m2/audit-ci.md).

## Plan

This is a one-off, off-cycle run of the [test cost audit](/quest/m2/audit-test-cost.md),
ahead of the cut. The recurring audit keeps its schedule, and this quest
creates no successor. Its PR rewrites that audit's baselines and its "check
first" list with what this run learned.

Decided 2026-11-01 (maintainer):

- Default to deleting. A test goes when its cost outweighs its signal, even
  if nothing else covers it. Only a test that earns its keep is refactored to
  be cheaper.
- Delete inline in this quest's PR, one commit per area. The PR body lists
  each deletion with its measured cost and why it wasn't worth it, so review
  catches a wrong call. Removing a public API or a user-facing feature is a
  follow-up through `/quest-plan`.

Start from the m2 audit's baselines and measurement pitfalls. Rank tests by
the slot time they take in a full-selection queue run and in Nightly, and
check Nightly's failure history for flakes. For each expensive test, name the
regression it alone would catch. If you can't, delete it. Watch for the same
behavior tested at several layers, such as a unit test, a session test, and
an end-to-end test of one property, and for tests that pin an old bug rather
than an edge case (AGENTS.md). A kept test still runs in CI, at least nightly.
