# [S] Aborted epoch reads keep their cause

## Goal

A track read through an epoch route that aborts after delivering ends with the
abort's own error (for example `FrameTooLarge`), not the `NotFound` a re-query
of the source returns.

## Plan

- Reproduced in [moq-dev/moq#6428](https://github.com/moq-dev/moq/pull/6428):
  `rs/moq-net/src/model/front.rs` (about line 563) returns a failed epoch route
  to Idle and redispatches it, and the re-query's refusal replaces the
  original error.
- Redispatch only when the error is a route failure (the `route_failed` rule in
  `rs/moq-net/src/model/resume.rs`); otherwise abort the read with that error.
  Check JS for the same path.
