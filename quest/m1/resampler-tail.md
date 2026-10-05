# [S] Flush the resampler tail when a sink finishes

## Goal

`moq play` plays a finite track to its last sample when its rate differs from
the device's. `Sink::finish()` flushes the partial input block held by the
sink's resampling channel, and its `Drain` resolves only after that tail has
played.

## Plan

- Flush on finish (decided 2026-10-04), not a documented limitation.
  #4624 added `Sink::finish(self) -> playback::Drain` and documents that the
  resampler's partial input block cannot be flushed; remove that note. The
  `fixed_resample` 0.13 channel is built in `channel()` in
  `rs/moq-audio/src/playback/sink.rs` and read by the mixer
  (`rs/moq-audio/src/playback/mixer.rs`); it has no flush.
- Pad the final partial block with silence so the mixer receives every
  resampled frame, then keep the existing device-deadline accounting for
  completion. The crate-private `crate::resample::Resampler` already has
  `flush`/`drain` for the same problem in decode and encode.
- Regression: a 44.1 kHz finite input into a 48 kHz mixer plays its full
  duration, next to `finish_plays_the_partial_final_period_before_completing`
  in `sink.rs`. The CLI drain tests use a fake sink that bypasses
  `moq-audio`, so they cannot catch this.

Public API: none. Wire: none.
