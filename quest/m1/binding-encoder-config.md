# [M] Binding encoders take exact rates and codec-only settings

## Goal

A binding caller can encode 30000/1001 video exactly, and a codec-only
encoder accepts nothing it ignores. `MoqVideoEncoderInput.framerate` becomes
a rational rate instead of a `u32` the encoders turn into `Rate(n, 1)`, and
the publication `track` name leaves `MoqVideoEncoderOutput` and
`MoqAudioEncoderOutput` for options only `MoqVideoProducer::new` and
`MoqAudioProducer::new` take.

## Plan

The draft checkpoint exposes `MoqVideoRate { numerator, denominator }` and
uses the existing `moq_video::Rate` validation. Codec-only and publishing
video paths share the same config conversion. C's current raw encoder also
uses a rational input; its track naming was already separate from codec
settings.

Both encoder output records now contain only codec settings. The proposed
`MoqAudioProducerOptions` / `MoqVideoProducerOptions` records own `input`,
`output`, optional `track`, and optional `bandwidth`. Producers take
`(catalog, options)` in UniFFI, Go, Kotlin, Dart, and C++. Python and Swift
keep their established keyword/labeled arguments and move track naming to
the producer, building the options record internally.

Remaining maintainer decisions, recorded on the draft PR:

- Recommend `video.Rate` (UniFFI `MoqVideoRate`) for the exact numerator and
  denominator record, matching Rust's existing role. `FrameRate` is the
  more explicit alternative.
- Recommend `audio.ProducerOptions` / `video.ProducerOptions` with input,
  codec output, and publication ownership together, so construction takes
  two arguments. The alternative is a publication/config record containing
  input, output, and track with bandwidth kept as a third argument.

Settle those names and constructor shapes before
retiring this quest. Keep the independent video catalog preference change
in [moq-dev/moq#6410](https://github.com/moq-dev/moq/pull/6410); it does not belong to encoder configuration. The
[OBS moq-video adapter](/quest/m2/obs-moq-video/adapter.md) later adds a preset
to the codec-only config; this shape is the one it extends.

Public API: breaking in moq-ffi, C's raw video input, and every binding
wrapper, with migration entries and regenerated bindings. Wire: none.

## Related

- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - the other exact-time break to the same bindings in this cut
