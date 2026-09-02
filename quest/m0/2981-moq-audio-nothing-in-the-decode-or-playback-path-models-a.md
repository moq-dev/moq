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
- `encode::Producer` needs no such correction: it anchors its epoch to the
  first input timestamp and advances by emitted samples, and
  `Resampler::process` already drops the startup delay from its output, so the
  first emitted sample lines up with the first input. `skipped()` exists for
  `decode::Consumer`, which reconstructs each batch's time from a later packet
  timestamp. Pin that with a test rather than changing it.

Exact contiguity cannot be the test. RTMP stamps in milliseconds and an AAC
frame at 44.1 kHz is 23.22 ms, so every packet on the most common ingest path
reads as discontinuous under a strict rule.

- One stated policy in `decode::Consumer`: a packet counts as discontinuous
  when its timestamp differs from the tracked tail by more than the source's
  timestamp quantization, one unit of the coarsest timescale on the path (one
  millisecond for RTMP), plus rounding. The tolerance cannot derive from a
  frame duration: one lost frame lands exactly one frame off, and Opus packets
  vary between 2.5 ms and 60 ms with no fixed duration in the catalog, so a
  half-frame rule from a 20 ms neighbour would splice across a lost 2.5 ms
  packet. Quantization is the only slack the stamps actually carry.
- On a gap: flush the resampler's pending samples as their own frame (they
  belong before the hole) or drop them, reset it, and stamp the next output
  from the new packet rather than by rewinding.
- `play_audio` writes silence for the missing duration, which also covers
  `latency_max` skips.
- Tests: frames at 0 and 2048 samples across a gap produce a hole; a 2.5 ms
  Opus packet lost after a 20 ms one is a gap; millisecond-stamped 1024-sample
  AAC packets detect no gap; the encoder's first PTS matches its first input.

## Closes

- [#2981](https://github.com/moq-dev/moq/issues/2981) - close this issue when the quest finishes
