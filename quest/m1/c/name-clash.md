# [S] One C name per moq-ffi item

## Goal

The generated C header gives every moq-ffi item its own name. Today the method
`MoqAudioEncoder::finish` and the record `MoqAudioEncoderFinish` both map to
`moq_audio_encoder_finish`.

## Plan

Either rename one of the two in moq-ffi, or fix the C backend in
[kixelated/rust2](https://github.com/kixelated/rust2)'s UniFFI fork so a method
and a type can never map to the same C name. Ask the maintainer which before
starting.

Public API: a rename in moq-ffi changes that item's name in every binding (Python,
Swift, Kotlin, Dart, Go, C++, and C); a generator fix changes only the C names of
the pairs it renames. Wire: none.
