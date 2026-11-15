# [M] A live HLS segment is listed once its last group is placed

## Goal

A live moq-hls media playlist lists a segment as soon as the export can know
the segment is complete, instead of one group later. On a 2 s GOP whose
timestamps fall off the grid lines, a segment is listed within one group of
its end, not two, and segment numbering stays the same on every edge.

## Plan

Measured 2026-11-15 on a live 720p30 RTMP source with a 2 s GOP: the newest
segment appeared in the media playlist about 2 s after its end by
`EXT-X-PROGRAM-DATE-TIME`, while MoQ subscribers received the same frames in
about 130 ms. Every HLS player's delay carries that extra group.

The cause is the grid rule from
[moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652)
(`rs/moq-hls/src/export/grid.rs`): a group belongs to the first grid line it
contains, so placing a group needs its end, the next group's pts, and a
segment completes only when a later group is placed in a later segment.
With groups one interval apart and an arbitrary phase, segment N is placed
when group N+1 starts, but only listed when group N+2 starts. Only a group
starting exactly on a line is placed without its end, and wall-clock
timestamps (RTMP ingest) almost never do.

The 2x `EXT-X-TARGETDURATION` stays as decided on 2026-11-03 (reaffirmed
2026-11-15, maintainer). This quest changes only when a segment is listed.

Two approaches:

- **Bound a group's end early.** A pending group whose frames reach the next
  grid line contains it, so it is placed in that line's segment, and the
  open segment completes then, without waiting for the next group. Grid
  rules, numbering and `Grid::starts` stay as they are. It saves the GOP's
  phase against the grid, between 0 and one interval (about 1 s on average
  for a random phase); a group starting just after a line saves almost
  nothing. It needs the pending group's newest frame pts, which the export
  does not read today (it reads only each group's first frame), so the cost
  of reading frame timestamps per group is part of the work.
- **Number a group by the slot it starts in** (`floor(pts / length)`). A
  segment completes as soon as a group starts in a later slot, so it saves
  the whole group. It renumbers segments, which breaks URLs across a mixed
  fleet during a roll. It also loses `Grid::starts`: an edge or recorder
  joining mid-stream can no longer tell that its first segment is whole
  without the group before it.

Recommended: bounding the end early. It keeps the numbering every edge and
recording agrees on, and gives back part of the delay without a wire or URL
change.

Tests (the grid's unit tests, on a controlled clock): a group whose frames
cross the next line completes the open segment before the next group starts;
a short group that ends before the line still joins the open segment; a
segment's `EXTINF` and number are unchanged from today's for the same groups.

Public API: none expected. Wire: none.

## Waiting

- Maintainer: pick the approach above (recommended: bound a group's end
  early), and record it in this Plan.
