# [S] moq-mux's container consumer tests run on the simulated clock

## Goal

The tests in `rs/moq-mux/src/container/consumer.rs` that depend on time run
under `#[kio::test]`'s simulated clock instead of `#[tokio::test]` on the real
one, and `gap_in_group_sequence_recovery` stops flaking in the merge queue.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md):
queue run 43557751216 failed `gap_in_group_sequence_recovery` ("Expected >= 4
frames, got 2", 0.386 s) on a quest-only PR the day after this epic's three
loaded runs passed. It is a plain `#[tokio::test]` with a 100 ms max delay,
like most of the file's 115 tests, about 40 of which set a millisecond
`with_max`. `KIO_DENY_HOST_TIMERS` catches timers fired on the host clock, not
reads of it, which is the gap REVIEW.md's Tests section names. Convert the
time-dependent tests, as `rs/AGENTS.md` asks; tests with no timing stay as
they are.

Public API: none. Wire: none.
