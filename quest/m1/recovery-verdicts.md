# [S] Group recovery refetches only on errors a new copy can fix

## Goal

A resumed group's recovery asks a route for a new copy only when the failure
was about delivery (the route or session went away, the stream was reset by a
peer that is leaving), never when it was about the group itself and would
repeat on every copy. Such a failure ends the group with its error once,
instead of looping on refetches. Rust and JS agree.

## Plan

From the follow-ups of [#5878](https://github.com/moq-dev/moq/pull/5878),
decided with the maintainer on 2026-10-20. Rust `Recover::wants`
(`rs/moq-net/src/model/resume.rs`) and JS `verdict` (`js/net/src/resume.ts`)
treat `Old`, `Evicted`, and an application code as verdicts, and JS also
`TooFarBehind`, which Rust does not; every other error is read as no verdict
and refetched. An error that repeats on every copy therefore loops. #5878's eviction reports `Evicted`, a verdict, so it no
longer triggers this, but other errors still can.

- Classify by error, with one list mirrored in Rust and JS, settling
  `TooFarBehind` on both sides. Keep the existing rule that an error from a
  copy whose route failed is never a verdict. Rust `route_failed` is
  narrower than JS `ROUTE_FAILURES`, and adding the JS guard in Rust hangs
  `partial_group` ([moq-dev/moq#6143](https://github.com/moq-dev/moq/pull/6143));
  align the two lists, fixing that hang at its cause.
- A content error from one route may not repeat on another. Settle whether
  such an error may move to a different route once; it never asks the same
  route again.
- A hostile route can answer every refetch with bad data. Bound that by the
  classification itself, not by a retry count.

Regression tests on mock time, Rust and JS: a group whose every copy fails
with the same content error ends once with that error, a route-loss failure
still recovers the group on the next route, and both languages classify every
listed error alike.

Public API: none. Wire: none.
