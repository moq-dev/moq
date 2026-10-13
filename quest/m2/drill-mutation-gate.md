# [M] Drill mutations prove sensitivity on the PR that could break them

## Goal

A PR that stops a drill mutation from proving sensitivity turns a CI check
red before merge, rather than turning the nightly
`Tests (test drill-sensitivity)` job red after it lands. The check is
advisory until the drills run on a controlled clock, then required.

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
- **Advisory until controlled time:** the drills are not deterministic yet.
  `rs/moq-relay/tests/drills.rs` uses real sockets and wall-clock sleeps and
  timeouts, and the impaired lane (which `ingress-burst-never-collected`
  runs) draws a random `MOQ_SHAPER_SEED` unless one is set. A mutated run
  can fail for the wrong reason under load. Pin the seed in the gate anyway,
  so the impaired lane's shaper decisions replay; socket timing and load
  still won't. Flip the check to required once the drills
  move onto [Controlled time](/quest/m1/time/README.md)'s clock. Blocking on
  that was rejected so the signal exists sooner. Reversed on review: the
  first pass chose required on the false premise that the drills already
  ran on mocked time.
- **Cost, measured 2026-10-11:** 62 of 973 commits on `main` in the prior two
  weeks touched a target file, 42 of them `rs/moq-net/src/lite/subscriber.rs`.
  The 2026-10-11 nightly ran five mutations in about 18 minutes.

Implementation guidance:

- Shape the job so it can later become required: it always runs and passes
  quickly when nothing is selected, because branch protection only accepts a
  check that reports.
- Run on `merge_group` too, selecting against its base SHA like
  `.github/workflows/check.yml`, so merge-queue commits get the result a
  required check needs. Reuse that workflow and `sensitivity.sh`'s
  mutation-name arguments rather than a parallel CI framework.
- Making the check required is a repository setting. Once the drills are on
  a controlled clock, ask the maintainer to add it.
- Keep the nightly full run: it still catches drift from changes the
  trigger misses, such as a refactor in a file the patch doesn't name.

Public API: none. Wire: none.

## Related

- [More tests under load](/quest/m1/test-flakes-2/README.md) - the other test-reliability work in this milestone
- [Controlled time](/quest/m1/time/README.md) - puts the drills on a controlled clock, which lets this check become required
