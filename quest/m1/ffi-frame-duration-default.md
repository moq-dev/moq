# [XS] FFI audio encoder takes the codec's own frame by default

## Goal

`MoqAudioEncoderOutput::frame_duration_us` defaults to 0, the
codec's own frame, so `MoqAudioCodec::aac()` works without an explicit 0 in
every binding. Opus still encodes 20 ms frames by default.

## Plan

- Decided in [#4183](https://github.com/moq-dev/moq/pull/4183): the uniffi
  default moves from 20000 to 0. Changing a published binding default is a
  break. Split from AudioToolbox encode on 2026-09-30.
- `rs/moq-ffi/src/audio.rs` already treats 0 as the codec's frame; the
  `default_frame_duration_matches_moq_audio` test that pins 20000 to
  moq-audio's default goes away with the literal.
- Update the wrappers and docs that restate 20000: the Python test asserting
  the default, `py/moq-rs/README.md`, and `doc/lib/{py,swift,kt,go,dart}`.
  libmoq already reads 0 as the default.

Public API: breaking default in moq-ffi and every binding. Wire: none.
