# [S] lite-03/04 deliver SUBSCRIBE FIN before a graceful close

## Goal

On lite-03 and lite-04, a graceful session close at a relay delivers every
pending SUBSCRIBE FIN before the session closes, so a track that finished
reads as finished downstream, not aborted.

## Plan

Decided with the maintainer on 2026-10-24: m1, in this epic, because
broadcasts now end by cause, and a dropped FIN turns a clean end into a
failure for followers on published lite versions.

- Found by the `relay_upstream_close` test in
  [moq-dev/moq#6660](https://github.com/moq-dev/moq/pull/6660): on lite-03/04
  the ACK drain closes first, so a SUBSCRIBE FIN still pending is dropped.
  Later lite versions deliver it.
- Fix the close ordering at its source for those versions; no extra wait or
  timer. Extend `relay_upstream_close` to leave several SUBSCRIBE FINs
  pending at the close and assert every track finishes, on lite-03 and
  lite-04 too.
