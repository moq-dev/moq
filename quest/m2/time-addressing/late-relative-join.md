# [M] A later reader's relative start on a partial lite-07 copy reads only its groups

## Goal

A reader that joins a lite-07 received copy already holding part of the
track, with a relative start, reads the groups its publisher counts back to
and nothing below them, in Rust and `@moq/net`. Today it resolves the start
against the groups the copy holds before SUBSCRIBE_STARTED can report the
publisher's: over a copy holding groups 1 and 2 of 0..9, a second reader of
`[{relative:2..time:8}]` reads 1, 2, 7, 8, then 3, 4, 5 and on, where it
should read 7 and 8 and end.

## Plan

Found by [moq-dev/moq#9132](https://github.com/moq-dev/moq/pull/9132), which
reports each live range's relative start on the copy
(`TrackState::relative_starts` in `rs/moq-net/src/model/track.rs`) but only
helps a reader whose start resolves after the report lands: the first reader,
whose relative start waits on SUBSCRIBE_START. A later reader's window
resolves at once (`Window::new` in `model/subscription/window.rs`), and
`Holdings::back` counts the copy's cache.

- Reproduce first: the second-reader case above, as a `range_anchors.rs`
  integration test, and the same in `js/net/src/integration.test.ts`.
- Recommendation: on a received copy, count back over the cache only where
  the copy holds every group from the counted one to its latest (received
  contiguously since it went live), and otherwise leave the relative start
  waiting, as a time start waits (`Start::Waiting`), until the range's
  SUBSCRIBE_STARTED reports it. Rejected: always waiting on the report,
  since an update that coalesces the reader's range into another one
  (`fit` in `lite/subscriber/fit.rs`) gets no SUBSCRIBE_STARTED of its own and would hang.
- The waiting reader must still settle when no report can come: the copy
  ends, or its feed goes idle.
