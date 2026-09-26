# [M] Consumers see the producer's real error, never Dropped

## Goal

A track or broadcast that ends because its source ended reports the source's
own error to every consumer, locally and across a relay. `Dropped` means only
that a handle was dropped without an end, which a correct producer never does.
#4179 fixed one path (a revoked upstream subscription now reads
`Unauthorized`); the rest still surface `Dropped`.

## Plan

- Known sources, from #4179: a source closing, a route leaving the origin's
  table, and a withdrawn source broadcast. Find each place a consumer can
  observe `Dropped` and make the ending side carry its real error (an explicit
  `abort` or a preserved cause), at the source rather than by remapping at the
  consumer.
- moq-transport: a `PUBLISH_DONE` carrying Unauthorized arrives as
  `Error::Remote(1)`. Map it to the same error lite reports.
- Regression tests per path, each failing on `Dropped` today, in-process and
  over a mock session.

Public API: none expected; error values consumers observe change. Wire: none.

## Related

- [#4179](https://github.com/moq-dev/moq/pull/4179) - fixed the revoked-upstream path
