# [S] moq-c's server_accepts_a_session passes every run

## Goal

`cargo nextest run -p moq-c --all-features server_accepts_a_session` passes
every run. On main at `73d65edbf7` it fails about four runs in five: the
`assert_eq!(moq_consume_close(consumed), 0)` after the accepted session closes
(`rs/moq-c/src/test.rs`, `server_accepts_a_session`) gets `-21`. The failure is
a race between the session's close and the consumer's own close, either in
the test's ordering or in how `moq_consume_close` reports a consumer whose
session has already ended.

## Plan

- Reproduce with a loop of the single test, then find which side is wrong:
  the binding's code for a consumer whose session closed under it, or the
  test expecting `0` there. Fix it at the root, with no retry or sleep.
- Find the PR that introduced it by blaming the assertion and the close path.
  Recent candidates touching the test or the session close are
  [moq-dev/moq#8954](https://github.com/moq-dev/moq/pull/8954),
  [moq-dev/moq#8961](https://github.com/moq-dev/moq/pull/8961), and
  [moq-dev/moq#8949](https://github.com/moq-dev/moq/pull/8949).

Public API: none expected, unless the fix changes what `moq_consume_close`
returns for an ended session.
