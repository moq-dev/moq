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

- `stale` (`judge`, `queue` at `jitter.rs:982`, `:1024`): drops a frame that
  arrived before its deadline but was read after it. Test that such a frame
  never goes out past its deadline.
- `unheard_lag` (`jitter.rs:952`, `:1290`) and the horizon shift
  (`jitter.rs:967`, `:1040`): both keep a new generation behind a track sent
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
