# [S] Flush the resampler tail when a sink finishes

## Goal

`moq play` plays a finite track to its last sample when its rate differs from
the device's. `Sink::finish()` flushes the partial input block held by the
mixer's resampling channel, and its `Drain` resolves only after that tail has
played.

## Plan

- Flush on finish (decided 2026-10-04), not a documented limitation.
  #4624 added `Sink::finish(self) -> playback::Drain` and documents that the
  resampling channel (`fixed_resample` in `rs/moq-audio/src/playback/mixer.rs`)
  cannot flush its partial input block; remove that note.
- Pad the final partial block (or use the channel's flush, if it has one) so
  the mixer receives every resampled frame, then keep the existing
  device-deadline accounting for completion. `moq_audio::Resampler` already
  has `flush`/`drain` for the same problem elsewhere.
- Regression: a 44.1 kHz finite track into a 48 kHz device plays its full
  duration, extending the existing device-free CLI drain tests that fail on a
  shortened tail.

Public API: none. Wire: none.
