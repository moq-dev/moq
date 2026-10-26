# [S] Audio buffers are sized by data a publisher sent, not by its catalog

## Goal

A publisher cannot crash its viewers by declaring a huge sample rate or
channel count in an audio catalog. moq-audio's resampler and playback ring
are sized from the first decoded packet and the output device, so a PCM
rendition declaring 4 GHz and 8 channels allocates nothing it has not been
sent.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred and
spot-verified, inherited. Reaches `moq play`, the bindings, and moqsrc
through moq-audio.

- `rs/moq-audio/src/decode/consumer.rs:173` sizes the resampler window as
  `rate * 20 / 1000` frames and `resample.rs:87` allocates
  `channels * chunk_frames` f32 twice (the wrapper and rubato): about 5 GB
  for the example.
- The playback ring (`playback/sink.rs:516`, amplified by
  `rs/moq-cli/src/play/media.rs:512`) is sized at
  `in_rate * (latency + 3 s)`, about 100 GB, so the allocation aborts.
- The AAC AudioSpecificConfig escape (a 16.7M rate) gives about 400 MB per
  sink. `decode/backend/pcm.rs:17` trusts the catalog too.

Fix direction, with no rate-cap constant: a fixed frame count for the
resampler window, the resampler and ring built from the first decoded packet
(so data actually received pays for the channel count), and the playback
ring sized from the device side. A test builds a decoder from the hostile
catalog and asserts no allocation before data arrives.

Public API: none. Wire: none.
