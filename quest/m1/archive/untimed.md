# [M] Untimed tracks are recorded and replayed untimed

## Goal

`moq_archive::Writer` records a track with no timescale instead of refusing it
with `Error::Untimed`, so `moq export archive` works over moq-lite before 05
and moq-transport without TIMESCALE (every track on drafts 14-16).
`moq_archive::Reader` replays such a track untimed: its frames carry no
timestamp, never a send or arrival time standing in for one. A frame missing
its timestamp on a timed track stays malformed.

## Plan

Decided 2026-10-14 by the maintainer, from
[#4822](https://github.com/moq-dev/moq/pull/4822)'s untimed follow-ups: bump
the format so a timestamp is optional, rather than keep refusing.

- Timedness is a track property, so `.info` carries it: an absent `timescale`
  means the track's frames store no timestamps. Settle whether that is a new
  format version: `moq-archive` is on `release`, so recordings in version 2
  may exist, and the line's in-place exception
  ([archive](/quest/m1/archive/README.md), Compatibility) ends once a release
  ships recordings.
- The timeline is a time index (`pts`, `duration`, `durationMax`), which an
  untimed track can't fill. Decide its shape before coding: likely one record
  per group with no `pts` or `duration`, as a sparse track already cuts.
- Consumers that need media time refuse an untimed track loudly:
  `moq-hls` can't derive segments from it, and the clock mapping does not
  apply.
- Mirror the change in the Hang format and its docs in the same PR:
  `js/hang` (`timeline.ts`, the version in `catalog/archive.ts`), the
  draft's Recording section (`drafts/draft-lcurley-moq-hang.md`), and
  `doc/concept/hang.md`.
