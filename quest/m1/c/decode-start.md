# [XS] moq-c decoders choose where to start

## Goal

A moq-c decoder can read a finished track from its start: decoding a finished
Opus track yields its frames instead of none.

## Plan

Found by [moq-dev/moq#6745](https://github.com/moq-dev/moq/pull/6745):
`rs/moq-c/src/video.rs:953` and `rs/moq-c/src/audio.rs:503` hardcode
`Start::Latest`. Confirm whether that was intended for live playback; if a
caller needs the other start, expose it the way moq-ffi does, else document
why moq-c only plays live.

Public API: maybe a start option on the decode config. Wire: none.
