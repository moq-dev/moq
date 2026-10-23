# [M] moq play stays live through refresh recovery with two decoders

## Goal

`moq play` of a video rendition with `warmup` set behaves like the moq-ffi and
moq-c players, which run two decoders in `moq_video::decode::Consumer`:
it holds its latency target through a cold join or a skip, may show refresh
stripes from a min-delay decoder, and switches to the clean max-delay decoder
once it catches up. Playback of renditions without `warmup` is unchanged.

## Plan

- `moq play` does not use `decode::Consumer`. `rs/moq-cli/src/play/media.rs`
  opens a `decode::Sink` and a `moq_mux::container::Consumer`, and
  `rs/moq-cli/src/play/video.rs` runs its own loop with receive-time clock
  updates, `Gap`/`End` handling, encoded buffering, paced output, and a
  bounded surface. Setting an option cannot reach it.
- Reuse `decode::Consumer`'s pair and its switch rule
  (`rs/moq-video/src/decode/select.rs`) rather than writing a second
  selector. Either extract the pair below `decode::Consumer` so the paced
  loop drives it, or migrate the loop onto `decode::Consumer` once it exposes
  what pacing needs. Pick whichever keeps the clock, gap/end, pacing, and
  surface contracts intact, and record why.
- A CLI playback test over a synthetic `warmup` track: a cold join presents
  min-delay pictures and then max-delay ones, a skip recovers the same way,
  and pacing and A/V timing match single-decoder playback.

