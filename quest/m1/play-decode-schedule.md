# [S] moq play decodes on a schedule that survives rewinds and deep reordering

## Goal

`moq play`'s video path keeps every valid picture and drops only stale ones,
across a timeline rewind, a B-frame reorder deeper than 100 ms, and a decoder
batch larger than the queue. Three Codex findings on
[#4241](https://github.com/moq-dev/moq/pull/4241) went unanswered and all
still hold in `rs/moq-cli/src/play/video.rs`:

- After a discontinuity rewinds timestamps, pictures the decoder still holds
  from the old generation pass the `< floor` check (an old 10 s picture is
  above a new 0 s floor), get scheduled far ahead, and can make the cap evict
  valid new pictures.
- Decode waits until `DECODE_AHEAD` (100 ms) before a frame's presentation. A
  reference frame whose PTS sits far past the B-frames encoded after it
  delays them past their own presentation when reordering is deeper than
  that; H.264 allows a 16-frame DPB.
- `decoded()` inserts a whole batch under one lock and trims to
  `MAX_FRAMES` (3), so a decode or final flush that returns more than three
  pictures loses all but the newest three.

## Plan

- Rewind: identify stale output by generation, not timestamp order. Flush or
  reset the decoder at the break and discard what it returns, or tag frames
  with the generation they were submitted in.
- Decode ahead: schedule by the earliest presentation the pending access
  units can still produce, or size the lead from the rendition's catalog
  `jitter` (the reorder depth), rather than a fixed 100 ms.
- Batches: let the presenter drain a large batch rather than trimming it in
  one step, or apply the cap only when the window is actually stalled. The
  cap exists to bound memory while presentation lags, not to cut a flush tail.
- Regressions on the fake presenter/decoder rig from #4241: a rewind with
  pictures held across it, a stream with reordering deeper than 100 ms, and a
  flush returning more than three pictures.
