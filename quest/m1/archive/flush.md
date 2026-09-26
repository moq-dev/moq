# [M] Idle tracks still get recorded

## Goal

A recording stores every frame within a bounded wall-clock delay of its arrival
even when its track goes idle, and a caller can force one track's pending
record out with `flush()`. An append-only group that stops growing, or a track
whose publisher stops sending, no longer waits for the next frame or the end of
the recording.

## Plan

Decided:

- Time decisions use elapsed = max(wall clock, pts) everywhere, so the existing
  cutting rule does the work with no new setting: an open group idle past
  `duration_max` (10 s by default) splits between frames, and a finished sparse
  group (minimum 0) stores at once. Peer timestamps alone can never stall or
  skip a cut, since the local wall clock bounds them.
- The segmenter stays pure: it takes `now` as an input and exposes the next
  wall-clock deadline at which a record must close. Callers arm the timer. The
  moq-archive writer uses tokio time; moq-mux's `Recorder` must stay wasm-safe
  (moq-mux builds for wasm32), so it uses moq-net's runtime `Timer` or
  `web_async` time, never `tokio::time`.
- Manual flush is per track: `Control::flush(name)` on the writer, beside the
  broadcast-wide `cut`, and `Recorder::flush()` on a timeline.
- Tests inject `now` into the segmenter and run the writer on paused tokio
  time, per the repository's mock-time rule. Cover an idle open group, an idle
  track after a finished group, a manual flush mid-group, and a publisher whose
  timestamps run ahead of or behind the wall clock.

## Required

- [Rust per-track timelines](/quest/m1/archive/track-timeline/core.md) - the per-track segmenter and writer this extends
