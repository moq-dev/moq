# [S] moq import ts: every elementary stream re-anchors below the live edge

## Goal

`moq import ts` keeps publishing across a content join, a playout loop wrap, or
a flagged encoder restart instead of exiting with `TimestampRewind`. Today only
legacy MPEG audio re-anchors; H.264, H.265, AAC, Opus, verbatim PES, and
sections abort the import the first time a timestamp lands below the producer's
live edge, which #3798 measured about 10 minutes into a looped channel. TS
ingest is a trusted source, so it shifts forward rather than refusing. The
producer contract does not change.

## Plan

Known triggers, none yet reproduced in-tree:

- B-frames: the live edge is the highest PTS written, so a new IDR after a
  join can land a frame below the last P-frame's PTS on a continuous PCR.
- A flagged PCR discontinuity that restarts lower: `timebase_break` resets the
  unwrapper, but `Producer::discontinuity()` never lowers the edge, so every
  non-legacy track still aborts.
- Sections take `last_pts` from the latest video PES, which is a B-frame's
  lower PTS in decode order, and `ZERO` right after a timebase break.

Work in `rs/moq-mux/src/container/ts/import.rs`:

- Reproduce first: an H.264 fixture that loops with B-frames, with and
  without the PCR discontinuity flag, fails on HEAD.
- Lift `LegacyStream::reanchor` into one helper every stream kind applies
  before `write`, instead of copying it per arm. Make the shift cumulative:
  today it is set once, so a second unflagged loop wrap lands below the edge
  again. Grow the offset whenever the shifted timestamp is still below the
  edge; a discontinuity clears it.
- Sections re-anchor the same way, so an SCTE-35 cue after a B-frame publishes
  at the edge.
- Tests beside `pcr_discontinuity_breaks_every_track`: H.264 backward restart
  flagged and unflagged, two unflagged loop wraps in a row (legacy audio too), AAC one frame short of the loop period, verbatim PES
  below the edge, a section after a B-frame PES.

## Closes

- [#3798](https://github.com/moq-dev/moq/issues/3798) - close this issue when the quest finishes

## Related

- [#3489](/quest/m1/3489-ts-import-stream-liveness.md) - the same import loop, reporting per-stream liveness
