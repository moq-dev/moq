# [XS] The PCM encoder converts a frame in one pass

## Goal

moq-audio's PCM encoder (`rs/moq-audio/src/encode/backend/pcm.rs`) writes a frame's samples to its little-endian payload in one bulk conversion instead of growing the payload with `extend_from_slice` one sample at a time. Its output bytes are unchanged, its instructions per sample fall, and they stop shifting with LTO code generation.

## Plan

Found by [moq-dev/moq#7520](https://github.com/moq-dev/moq/pull/7520), whose `batches` bench uses PCM as the codec so buffering cost isn't hidden by encoder work. The per-sample `extend_from_slice` loop's instruction count moves with LTO code generation, which makes the PCM control noisier than it needs to be.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2. On little-endian targets the `f32` slice's bytes are already the payload, so copy them at once; keep a per-sample path only for big-endian, behind `cfg(target_endian = "big")`. Check the PCM decoder's mirror loop (`rs/moq-audio/src/decode/backend/pcm.rs`) and give it the same treatment if it has one. Measure with #7520's `rs/moq-audio/benches/batches.rs` count table (`--bench __none__`) once #7520 lands, and keep its `batch_size_does_not_change_the_published_track` test passing.

Public API: none. Wire: none.
