# [M] Drill mutations prove sensitivity on the PR that could break them

## Goal

A PR that stops a drill mutation from proving sensitivity fails a required CI
check before merge, rather than turning the nightly
`Tests (test drill-sensitivity)` job red after it lands.

## Plan

Why: #5054 (merged 2026-10-08) moved the sources' release out of what
`test/drill/mutations/subscriber-leaks-broadcasts.patch` leaked. The patch
still applied, so the `--apply-only` gate in `just check` (`check:drill` in
`sh/dispatch.sh`) stayed green. The mutation no longer caused a leak, and the
nightly stayed red for 3 days until #5338. `--apply-only` catches a patch
that stops applying, not one that applies but no longer reintroduces the bug.

Decided 2026-10-11:

- **Mechanism:** PR CI runs `test/drill/sensitivity.sh` for just the
  mutations the PR could break. Running it in local `just check` was
  rejected because it costs every developer a cold snapshot build. Alerting
  on a red nightly instead was rejected because it doesn't stop the merge.
- **Trigger:** a mutation runs when the PR changes a file its patch modifies
  (the patch's `+++ b/` paths), the patch itself, or
  `rs/moq-relay/tests/drills.rs`. A change to `test/drill/sensitivity.sh`
  runs every mutation.
- **Required, not advisory:** a stale mutation is a real coverage gap. The
  drills run on mocked time, so the job should be deterministic.
- **Cost, measured 2026-10-11:** 62 of 973 commits on `main` in the prior two
  weeks touched a target file, 42 of them `rs/moq-net/src/lite/subscriber.rs`.
  The 2026-10-11 nightly ran five mutations in about 18 minutes.

Implementation guidance:

- A required check has to report on every PR, so the job always runs and
  passes quickly when nothing is selected. Branch protection only accepts a
  check that reports.
- Making the check required is a repository setting. Ask the maintainer to
  add it once the job is green on `main`.
- Keep the nightly full run: it still catches drift from changes the
  trigger misses, such as a refactor in a file the patch doesn't name.

Public API: none. Wire: none.

## Related

- [More tests under load](/quest/m1/test-flakes-2/README.md) - the other test-reliability work in this milestone
