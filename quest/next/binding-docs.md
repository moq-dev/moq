# [S] The binding docs compile against the wrappers

## Goal

Every sample in `doc/lib/{py,swift,kt,c}` and the package READMEs names a
symbol that exists with the arity it shows. Twelve do not today.

## Plan

- `doc/lib/py/index.md`: `request_broadcast(pattern)` needs the announced
  prefix joined; `publish_video(VideoEncoderInput, ...)` is `encode_video`;
  `video.write` is `write_frame` on a `MediaProducer`; `route_updates()`
  does not exist.
- `doc/lib/swift/index.md`: `subscribeCatalog()` needs `await`;
  `publishVideo(input:output:)` is `encodeVideo`; `write` is `writeFrame`.
- `doc/lib/kt/index.md`: `publishVideo(input, output)` is `encode_video`.
- `doc/lib/c/index.md`: `moq_publish_video_raw` and the `_consume_*_raw`
  mirrors are `moq_encode_*`/`moq_decode_*`; the same page already uses the
  new names two bullets up.
- `py/moq-rs/README.md`, `py/moq-rs/docs/index.md`, `swift/README.md`,
  `kt/README.md`: `announcement.broadcast` (an `AnnounceUpdate` carries no
  broadcast), `publish_media`, `AudioFormat.s16` (a sample format), and the
  incomplete `connect` argument lists.
- Commits #3723, #3744, #3671, and #3410 touched no `doc/lib` page; add
  the `estimated_*` stats fields, `opus()`, and `frame_duration_us` where
  each binding documents them.
- A doc-check that extracts each page's samples and compiles them against
  the wrapper, nightly at least, so the drift cannot return.

Public API: none. Wire: none.

## Required

- [Binding parity](/quest/main/binding-parity.md) - document the surface after it settles
