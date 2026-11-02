# [M] Delete the tests that don't earn their cost

## Goal

Every test in the merge gate and in nightly is worth its cost. A test whose
wall time, CPU, or flakiness outweighs the regression it alone catches is
deleted. A costly test that earns its keep is made cheaper: fast at its
source, or moved to nightly. CI workflows and jobs belong to
[Delete the CI jobs that don't earn their cost](/quest/m1/prune-ci.md).

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

Audit implementation is checkpointed in
[moq-dev/moq#7600](https://github.com/moq-dev/moq/pull/7600): removed the
redundant paced CLI silent-video report test and its private TS suppressor
(6.932/6.948 hosted seconds; 7.763/7.152 local seconds). Mid-import and
late-join reports, stdin-EOF cases, and hostile-input cache bounds remain.
The recurring m2 audit has the refreshed baseline and ownership list in place;
no successor was created.

The kept import/report workspace selection passes after the cut (98 tests).
The standard `quest gate just ci` stops in Cargo documentation on mbx
`E2BIG`, without a launcher or environment bypass. Remaining: pass that gate
and complete `/quest-review` on the committed head before finishing this quest. The unchanged full Rust
selection exposed the known `quic_client_close_drains_finished_track`
`Unroutable` flake, already tracked by
[moq-dev/moq#7590](https://github.com/moq-dev/moq/pull/7590). CLI-only test
builds also fail on the existing unused `listener::Io` re-export; workspace
feature unification hides it. Do not remove either test or weaken checks to
clear those failures. The mbx environment defect remains owned by
[moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594).
