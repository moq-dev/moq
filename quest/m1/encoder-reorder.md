# [XS] moq-video encoders declare no reordering

## Goal

moq-video's encoders, which emit no B-frames, declare `reorder: 0` in the
`VideoHint` they publish, so readers take decode time from it instead of
guessing.

## Plan

[moq-dev/moq#6453](https://github.com/moq-dev/moq/pull/6453) adds
`VideoHint.reorder`, which `rendition_hint`
(`rs/moq-video/src/encode/producer.rs`) copies from the probed rendition, but
the encoders leave it unset, so their renditions stay on the inferred clock.
Declare `0` there, even when the SPS omits its bitstream restriction, and
assert it in an encoder test.

Public API: none. Wire: none.
