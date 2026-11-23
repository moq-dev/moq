# [S] A lite subscriber test fails beside the others in its process

## Goal

`moq-net`'s `lite::subscriber::test::refresh_joins_coalesce_upstream` passes
when it shares a process with the rest of its module, fixed at its cause.

## Plan

It passes alone, under nextest (one process per test, as CI runs it), and
with any `MOQ_SIM_SEED`, but fails in about 7 of 8 runs of
`cargo test -p moq-net --all-features --lib lite::subscriber` on `main`
(found 2026-11-23 while iterating
[moq-dev/moq#9422](https://github.com/moq-dev/moq/pull/9422)). A test that
depends on what ran before it in the process points at state shared across
tests, such as a `static`, a thread-local, or a clock left in place, which
nextest's isolation hides; find what leaks rather than isolating the test.
