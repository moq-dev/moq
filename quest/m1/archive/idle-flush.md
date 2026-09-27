# [M] Flush idle archive groups

## Goal

`moq_archive::Writer` stores a group that stops receiving frames within a
bounded time, instead of waiting for the next frame or the track's end. A
sparse append-only group, such as a log that goes quiet, is durable and
listed on its timeline while it is still open. An application can also flush
or cut one track on demand.

## Plan

- Drive each track's `Segmenter` from a timer as well as its frame reports: a
  record left open past a bound closes after its newest reported frame, the
  way `duration_max` splits a long group, and the group's later frames start
  the next record.
- Advance time on max(wall clock, pts), so a peer can neither stall the flush
  by withholding frames nor skip it by lying about timestamps.
- Add a per-track flush and cut to `moq_archive::Control` beside the
  broadcast-wide `cut`.

## Required

- [Per-track timelines](/quest/m1/archive/track-timeline/README.md) - the writer cuts and commits each track independently
