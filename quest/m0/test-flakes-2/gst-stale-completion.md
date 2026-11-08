# [S] moq-gst's stale-completion sink test survives load

## Goal

`rs/moq-gst/src/sink/imp.rs`'s `a_stale_completion_cannot_end_the_next_publication`
passes reliably under load, fixed at its cause, and its `#[ignore]` is
removed.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from
[moq-dev/moq#8160](https://github.com/moq-dev/moq/pull/8160)'s validation:
it failed once in a 30-iteration stress run of the moq-gst suite at
[8e2db7b9f6](https://github.com/moq-dev/moq/commit/8e2db7b9f609f1c0992049def7499c268ddcf085).
It is quarantined with `#[ignore]` until this quest fixes it, per AGENTS.md.

The test starts a session against an unreachable URL, stops it, starts a
second one, and asserts that failing the first session's completion leaves
the second's open and the pad relinked to it. Reproduce it under stress
first and record which assertion fails; that decides whether the sink or
the test is wrong. Candidates: a state transition that returns before the
old session's task lets go of the pad; a second session that hasn't linked
its pad by the time `Paused` returns; or the old session's status task
failing its own completion during shutdown before `stale.fail()` runs, which
breaks the test's assumption rather than the sink. Fix a sink defect in the
sink, never by waiting in the test, and prove it with a test that forces
the losing order.

Public API: none. Wire: none.
