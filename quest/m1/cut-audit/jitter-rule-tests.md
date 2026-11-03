# [S] Each export-clock rule kept by the jitter audit has its own test

## Goal

`stale`, `unheard_lag`, and the horizon shift in `rs/moq-mux/src/jitter.rs`
each have a test that fails when that rule alone is removed. Today the
`moq-mux` suite passes with any one of them knocked out. No rule's behavior
changes.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7412](https://github.com/moq-dev/moq/pull/7412).

Milestone: ✅ m1. Not chosen: m2.

[moq-dev/moq#7412](https://github.com/moq-dev/moq/pull/7412)'s knockout measurement kept these rules because each guards behavior
admission does not provide, but none has a test of its own:

- `stale` (`judge`, `queue` at `jitter.rs:992`, `:1025`, at be39188000):
  drops a frame that arrived before its deadline but was read after it.
  Test that such a frame never goes out past its deadline.
- `unheard_lag` (`jitter.rs:955`, `:1291`) and the horizon shift
  (`jitter.rs:970`, `:1041`): both keep a new generation behind a track sent
  later than the anchor, and in the tests each covers for the other
  (removing both fails `a_track_crossing_after_the_new_clock_keeps_its_lead`
  and one more). Find an input where only one applies for each, such as a
  track never heard during acquisition versus one heard but queued past the
  horizon.

Rerun the knockouts the way [moq-dev/moq#7412](https://github.com/moq-dev/moq/pull/7412) did (one lib run per rule removed) and
record in the PR which new test fails for each. If a rule turns out to have
no case only it covers, stop and report rather than delete it.

[moq-dev/moq#7412](https://github.com/moq-dev/moq/pull/7412) (merged) removed the `leapt` arm from the same file.

Public API: none (`jitter` is crate-private). Wire: none.

The three controlled-time tests are implemented in
[moq-dev/moq#7722](https://github.com/moq-dev/moq/pull/7722). Each individual
knockout fails only its corresponding new test; the other 1,989 lib tests
pass. Production code is unchanged. Full lib tests pass with all rules
restored (1,990 passed, 3 skipped).

Remaining: complete the full local CI gate after the unrelated base-branch
C-binding cancellation failure is resolved, then remove this quest and its
reference. `just ci origin/main` stopped at
`moq-c test::raw_track_subscription_options_and_update`, `test.rs:2283`:
cancelling after range completion returned `TRACK_NOT_FOUND` (-27), expected
0. The same failure reproduced at `origin/main` commit
`801fd9a2f2f5c88371fa833d805620d97d372465` on iteration 4 of a nextest stress
run. Repository lint checks passed. No C-binding fix belongs to this quest.

## Waiting

- moq-dev/moq: resolve the base-branch C-binding cancellation/completion race, then rerun `just ci origin/main` for the test-only PR. Recommend planning that fix separately rather than expanding this quest; the three jitter rules already have independent coverage.
