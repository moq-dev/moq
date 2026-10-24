# [S] One C name per moq-ffi item

## Goal

The generated C header gives every moq-ffi item its own name. Today the method
`MoqAudioEncoder::finish` and the record `MoqAudioEncoderFinish` both map to
`moq_audio_encoder_finish`.

## Plan

Decided with the maintainer on 2026-10-24: fix the C generator, not moq-ffi. The
generator derives collision-free names, for example by qualifying a method with
its type, so no moq-ffi item is renamed.

The generator lives in [kixelated/rust2](https://github.com/kixelated/rust2)'s
UniFFI fork (`uniffi/`), so the work lands there. This quest stands in for it
until moq builds from a fork that ships the fix.

Public API: the C names of the pairs the generator renames. Wire: none.
