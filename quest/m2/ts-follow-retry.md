# [S] The TS follower judges a publisher leaving by its error's retry answer

## Goal

`moq_mux::container::ts::follower::left` asks `moq_net::Error::retry`
whether the publisher left, instead of matching `moq_net::Error` variants,
so the TS export and moq-gst's `moqsrc` agree on what ends a run.

## Plan

`left` (`rs/moq-mux/src/container/ts/follower.rs`) matches `Unroutable`,
`Stream(Unroutable | Session(_))`, `Session(_)`, `Transport(_)`,
`IdleTimeout`, and `Remote(_)`. `retry() != Retry::Never` differs on two
groups, each pinned by a test in `export_test.rs`:

- `Cancel`, `Closed`, `GoingAway`, `Dropped`, and the stream codes
  `Internal`, `Cancel`, `GoingAway`, and `ControlTimeout` are a failed
  delivery to `retry` but the export's own failure to `left`
  (`a_follower_fails_at_once_on_its_own_failure`).
- `Remote(_)`, an unregistered moq-transport request code, is a verdict to
  `retry` but a publisher leaving to `left`
  (`a_follower_waits_out_the_follow_on_a_lost_route`). A relay's lost route
  no longer arrives this way: PUBLISH_DONE's INTERNAL_ERROR reads as
  `Unroutable` on both wires.

Recommended: adopt `retry`, treating a cancel or GOAWAY reset as the
publisher leaving as `moqsrc` does, and an unregistered moq-transport code
as the verdict it is to every other reader.

## Waiting

- Maintainer: whether a cancel, GOAWAY, or INTERNAL_ERROR reset counts as
  the publisher leaving (follow it) or as the export's own failure (end at
  once). Recommended: leaving, matching `moqsrc` and `Error::retry`.
