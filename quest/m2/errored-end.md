# [M] An errored track end keeps its history

## Goal

A track can end with an error and still serve the groups it holds or can fill:
a reader arriving later gets that history, then the error, instead of either
the error alone or a clean finish. `archive::Consumer` replays a stored
`{"error":code}` end this way, so a replayed recording ends the way its source
did.

## Plan

From [moq-dev/moq#6262](https://github.com/moq-dev/moq/pull/6262), decided by
the maintainer 2026-10-23: the consumer replays an errored end as a clean
finish after its newest stored group, plus a warn log, because
`track::Producer::abort` refuses every later reader the stored history. The
alternatives were an abort (loses history) and an abort only after the last
group (still refuses later readers). This quest replaces the clean finish.

- `moq-net`: an end that records an error alongside the final position.
  Readers below the end read groups and fills as after `finish_at`; a reader
  reaching the end gets the error instead of `None`. Unlike `abort`, the cache,
  fills, and the listing stay as after a finish. Pick the API shape in the PR
  with options and a recommendation, such as `finish_at` taking an end reason,
  and no `finish_with_x` variant.
- Carry it across a hop if a relay must forward it: moq-lite resets the
  subscription with the code today, which a later subscriber never sees. Any
  wire change goes in the next unpublished lite version and its draft.
- `archive::Consumer` maps `End::Error(code)` to the new end and drops its
  warn log.

Test a consumer replaying an errored track: a subscriber from the oldest group
reads every stored group and then the error; a later fetch still reads stored
groups.

Public API: a `track::Producer` end carrying an error, and what a reader
observes at it. Wire: possibly a lite track end carrying an error code.

## Related

- [DVR rewind](/quest/m2/archive-dvr.md) - seeks back through an archive whose source may have ended with an error
