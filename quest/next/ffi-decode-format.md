# [S] The decode pixel format reaches every uniffi binding

## Goal

A Python, Swift, Kotlin, Go, or Dart consumer chooses I420 or RGBA decode
output the way a C consumer does. `moq_video_decoder_output` in libmoq
carries `format`, `width`, and `height` (#3674), while
`MoqVideoDecoderOutput` in `rs/moq-ffi/src/video.rs` has only a best-effort
`resize` and documents that there is no format to choose.

## Plan

Add `format: MoqVideoPixelFormat` with an I420 default to
`MoqVideoDecoderOutput`, reusing the enum the encode side already exports,
and make the delivered frame carry the format it was decoded to. Keep
`resize` best-effort but state per backend which honor it. Follow the
Cross-Package Sync row for `rs/moq-ffi`: libmoq maps its struct onto the
shared record instead of a private conversion, the Go wrapper and the
hand-written Python and Dart wrappers gain the field, and each
`doc/lib/*/index.md` gets the "Raw decode output" bullet the C page has.
Test one RGBA decode per binding smoke suite.

Public API: additive field on a uniffi record and its wrappers. Wire: none.
