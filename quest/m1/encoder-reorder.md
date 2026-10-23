# [XS] moq-video encoders declare no reordering

## Goal

moq-video's encoders, which emit no B-frames, declare `reorder: 0` in the
`VideoHint` they publish, so readers take decode time from it instead of
guessing.

## Plan

[Decode order](/quest/m1/ts-reliability/decode-order.md) adds
`VideoHint.reorder` and lists the moq-video encoders as a producer that
declares it, but leaves them out. Set it in `rendition_hint`
(`rs/moq-video/src/encode/producer.rs`) and assert it in an encoder test.

Public API: none. Wire: none.

## Required

- [Decode order](/quest/m1/ts-reliability/decode-order.md) - adds the `reorder` field
