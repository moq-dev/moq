# [M] moq-audio: model a media gap in decode and playback

## Goal

A discontinuity in audio frame timestamps is a hole in the output, not a
splice: the resampler never carries samples across it, playback fills it with
silence so the sink stays aligned with media time, and the encoder's published
timestamps say where its samples actually belong.

## Plan

Gaps are routine: `latency_max` skipping a stalled group, `moq play` dropping
an undecodable packet, an ingest that resyncs. Below the catalog they are all
treated as contiguous audio.

- `decode::Consumer` in `rs/moq-audio/src/decode/consumer.rs` tracks a tail but
  never compares an incoming timestamp against it; it only reacts to a
  *declared* discontinuity through the container counter. `resample::Resampler`
  already has `reset` for exactly this and drops its own startup delay, so the
  missing piece is the decision to call it.
- `play_audio` in `rs/moq-cli/src/play.rs` writes PCM back to back into a
  clockless `playback::Sink`, so a gap shortens the audio; the A/V clock
  re-anchors within one buffer, with video running ahead until then.
- `encode::Producer` uses the same resampler but never reads `skipped()`, so
  every resampled publisher's PTS carries the filter delay as a fixed offset
  (about 1.4 ms at 44.1 to 48 kHz). `decode::Consumer` compensates; the
  encoder should too, in the same change, since both are about the resampler
  telling its caller where its samples belong in time.

Exact contiguity cannot be the test. RTMP stamps in milliseconds and an AAC
frame at 44.1 kHz is 23.22 ms, so every packet on the most common ingest path
reads as discontinuous under a strict rule.

- One stated policy in `decode::Consumer`: a packet counts as discontinuous
  when its timestamp differs from the tracked tail by more than half a codec
  frame at the track timescale, derived from the frame size and timescale
  rather than a constant. One lost frame lands exactly one frame off, so the
  tolerance has to sit well below a frame; millisecond rounding is under
  1 ms against a 23 ms frame, so it stays well inside.
- On a gap: flush the resampler's pending samples as their own frame (they
  belong before the hole) or drop them, reset it, and stamp the next output
  from the new packet rather than by rewinding.
- `play_audio` writes silence for the missing duration, which also covers
  `latency_max` skips.
- `encode::Producer` subtracts the resampler's skipped frames from its epoch
  so the first published frame is stamped where its input was.
- Tests: frames at 0 and 2048 samples across a gap produce a hole; millisecond
  stamped 1024-sample AAC packets detect no gap; the encoder's first PTS
  matches its first input.

## Closes

- [#2981](https://github.com/moq-dev/moq/issues/2981) - close this issue when the quest finishes
