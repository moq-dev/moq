# [S] One front pass handles every ready track query

## Goal

A front's route swap costs time linear in its tracks. `origin/copy_walk`
(#4995) measured it growing roughly with the square of the track count:
186 µs at 32 tracks, 769 µs at 64, 2.16 ms at 128 (one copy each), on track
for about 10 ms at a few hundred.

## Plan

In `run_front`'s wait loop (`rs/moq-net/src/model/origin.rs`, around the
query handling), each pass handles one ready track query and then rescans
every track from the start. Handle every ready query in one pass. Measure
with `origin/copy_walk` before and after; the per-copy cost (about 2 µs for
32 extra copies) is out of scope until a profile says otherwise.

