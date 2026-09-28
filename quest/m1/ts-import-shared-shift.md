# [S] moq import ts: one re-anchor shift per program

## Goal

An unflagged loop wrap in `moq import ts` moves audio and video forward by the
same amount, so A/V sync holds across any number of wraps. Today each
elementary stream owns a `Reanchor` and grows its shift to reach its own live
edge ([#3997](https://github.com/moq-dev/moq/pull/3997)). Audio and video
edges sit on their own last frame starts, so each wrap drifts A/V by the
difference in frame durations: about 12 ms at 30 fps with 48 kHz AAC, or
1.7 s a day on a 10-minute loop.

## Plan

Decided: a program-level shared shift in `rs/moq-mux/src/container/ts/import.rs`.
Every stream of a program applies the same shift, grown once per wrap by the
largest amount any stream needs to clear its edge, which preserves the source's
inter-stream offsets. A timebase break (PCR discontinuity) clears it for the
whole program, as it already does per stream.

Guidance:

- The shift must be known before any stream emits a frame of the new
  generation (Codex on #4307): a first stream that grows only enough for
  itself leaves a later-arriving stream below its edge, and growing again
  then drifts the two. Decided: hold each stream's post-wrap frames until
  every live stream of the program has shown its new PTS, then take the
  maximum growth once.
- The hold needs its own bound: #3489 adds per-PID counters, not a timeout,
  and the catalog `stalled` bit (`Stream::tick`, #3630) covers video only and
  runs on the catalog's timer. Decided: bound the hold on the program clock
  (PCR advance since the first stream's new generation, not wall time), and
  commit the shift over the streams seen so far when it expires. A stream
  that returns later applies the committed shift, clamped to its edge as
  below. The `Anchor`/`Lane` split behind `live()` in `moq_mux::clock` solves
  a similar problem for restarts, but the remove-live quest deletes it, so
  copy what helps rather than depending on it.
- A stream whose own edge is still above the shifted timestamp after the
  shared growth (its tail ran longer) is the case that forces growing by the
  maximum. Landing on its edge is accepted today; keep that trade-off.
- Sections already take the video shift, so cues keep following pictures.
  MPEG-2 video (`Stream::Clock`) has no track and so no edge, but it should
  read the shared shift too.
- Tests beside the existing loop-wrap tests: a muxed H.264 + AAC loop whose
  period is not a multiple of either frame duration keeps the first audio and
  video timestamps of each pass at the source offset across three wraps.

## Related

- [#3489](/quest/m1/3489-ts-import-stream-liveness.md) - per-PID liveness in the same importer; touches `Stream` but not the shift
- [Remove live()](/quest/m1/remove-live.md) - deletes the restart anchor; a wrap shift stays input-derived
