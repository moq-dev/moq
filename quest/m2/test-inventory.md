# [M] Every opt-in test target has a CI caller

## Goal

The merge gate fails when a test that CI does not pick up by default has no
CI caller, so AGENTS.md's "every test runs in CI, at least nightly" is
enforced instead of rediscovered by each coverage audit.

## Plan

From the 2026-10-27 coverage audit. nextest and `bun test` find ordinary tests
by themselves; the gaps are the opt-in ones. The m1
[coverage audit](/quest/m2/audit-test-coverage.md) found uncalled VA-API
tests by hand, and `js/common/worklet-matrix.ts` still says "Not in CI".

Decided 2026-10-27 (maintainer): check the opt-in targets, not everything.

- Every `#[ignore]` test is matched by some recipe's `--run-ignored` filter.
- Every fuzz target and every bench target runs in some workflow.
- Every `just` recipe that runs tests is reachable from a workflow.
- Every standalone test script (like `worklet-matrix.ts`) has a caller.

It runs in the merge gate beside the CI-selection tests, and reports each
orphan by name with where it was found. An intentionally local-only target
needs an explicit allowlist entry that names its owning quest. After this
lands, the coverage audit's `#[ignore]` walk can rely on the check.

Public API: none. Wire: none.

## Related

- [Test coverage audit](/quest/m2/audit-test-coverage.md) - the recurring audit this automates part of
