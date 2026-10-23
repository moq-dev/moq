# [M] Binding encoders take exact rates and codec-only settings

## Goal

A binding caller can encode 30000/1001 video exactly, and a codec-only
encoder accepts nothing it ignores. `MoqVideoEncoderInput.framerate` becomes
a rational rate instead of a `u32` the encoders turn into `Rate(n, 1)`, and
the publication `track` name leaves `MoqVideoEncoderOutput` and
`MoqAudioEncoderOutput` for options only `MoqVideoProducer::new` and
`MoqAudioProducer::new` take.

## Plan

Decided 2026-10-22 (maintainer, from an external API audit): one quest for
both, since they break the same records and every wrapper, so callers
migrate once with the cut.

- Rate: `rs/moq-ffi/src/video.rs` builds `moq_video::Rate::new(framerate, 1)`
  in `video_encoder_config` (the codec-only `MoqVideoEncoder`) and again
  inline in `MoqVideoProducer::new`; `moq_video::Rate` already holds a
  numerator and denominator. Expose it as a numerator/denominator record and
  build the rate once. Audio's integer `sample_rate` is exact and stays.
- Publication options: `track` is the only publish-only field today.
  `video_encoder_config` and `audio_settings` never read it. Nest it in a
  publication options record taken by the producers, beside the existing
  `catalog` and `bandwidth` arguments, rather than adding a fourth argument.
  Keep the `audio`/`video` namespaces. Propose the record and field names
  with a recommendation in the PR before shipping them.
- Wrappers alias the uniffi records, so update `py/moq-rs`, `kt/moq`,
  `swift`, `dart/moq`, `go/wrapper`, `cpp/moq`, their tests and `doc/lib/*`
  per the Cross-Package Sync table. The generated C picks the records up;
  the hand-written `rs/moq-c` has no raw encoder, so it is untouched.
- [OBS moq-video adapter](/quest/m2/obs-moq-video/adapter.md) later adds a
  preset to the codec-only config; this shape is the one it extends.

Public API: breaking in moq-ffi and every binding wrapper, each with a
`MIGRATE.md` entry. Wire: none.

## Related

- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - the other exact-time break to the same bindings in this cut
