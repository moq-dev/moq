# [M] Bound the inferred decode clock

## Goal

An MKV or TS export of a video rendition that declares no catalog `reorder`
holds a bounded number of frames in its decode clock, whatever its timestamps
do: flat or backward PTS cannot bypass the export's backpressure or drain
quadratically.

## Plan

[moq-dev/moq#6453](https://github.com/moq-dev/moq/pull/6453) bounds a
rendition that declares `reorder`: its `DeclaredClock` holds at most that many
frames and refuses deeper reordering. The importers now declare it from the
SPS, so the remaining path is a catalog that omits it while the stream
reorders: a hostile or older publisher, or field-coded H.264.

The maintainer kept that absence on today's inferred clock
(`DecodeClock::pop` and `pop_counted` in `rs/moq-mux/src/container/decode.rs`).
It settles a slot once the highest PTS read is the lookahead past it, so PTS
that stop advancing hold every frame read since. The bound must keep the DTS
a conforming source gets today; a held count tied to the inferred depth, or to
the lookahead in frames, are the candidates. Sweep held frames over frame
count and payload size through MKV and TS export, as the declared path's
tests do.
