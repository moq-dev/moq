# [M] Idle tracks still get recorded

## Goal

A recording closes the record holding every frame within a bounded wall-clock
delay of its arrival, and hands it to the committer, even when its track goes
idle, and a caller can force one track's pending
record out with `flush()`. An append-only group that stops growing, or a track
whose publisher stops sending, no longer waits for the next frame or the end of
the recording.

## Plan

Decided:

- Time decisions use elapsed = max(wall clock, pts) everywhere, so the existing
  cutting rule does the work with no new setting: an open group idle past
  `duration_max` (10 s by default) splits between frames, and a finished sparse
  group (minimum 0) stores at once. Peer timestamps alone can never stall a
  cut, since the local wall clock bounds them. A forward pts leap can close a
  record early; a new record then starts at the leaping frame, so each leap
  costs at most one extra record. Say in the PR how repeated leaps are bounded.
- The segmenter stays pure: it takes `now` as an input and exposes the next
  wall-clock deadline at which a record must close. Callers arm the timer. The
  moq-archive writer uses tokio time; moq-mux's `Recorder` must stay wasm-safe
  (moq-mux builds for wasm32), so it uses `web_async` time, never
  `tokio::time` (moq-net's runtime `Timer` went away in #4437).
- Manual flush is per track: `Control::flush(name)` on the writer, beside the
  broadcast-wide `cut`, and `Recorder::flush()` on a timeline.
- Rust only (decided 2026-10-07): JS publishes no timeline yet, so its idle
  deadline and `flush()` belong to
  [JS per-track timelines](/quest/m1/archive/js-timelines.md), and this quest
  does not wait on the port.
- The bound covers closing a record, not upload latency: a slow object-store
  PUT still delays the durable commit, which the writer already serializes.
- Tests inject `now` into the segmenter and run the writer on paused tokio
  time, per the repository's mock-time rule. Cover an idle open group, an idle
  track after a finished group, a manual flush mid-group, and a publisher whose
  timestamps run ahead of or behind the wall clock.

## Related

- [JS per-track timelines](/quest/m1/archive/js-timelines.md) - carries the JS idle deadline and `flush()`
