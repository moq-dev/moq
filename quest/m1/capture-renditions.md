# [M] Capture publishes scaled simulcast renditions

## Goal

`moq import capture --rendition sd=0.25` publishes `video/sd` next to
`video` in the same broadcast, the way js/publish's simulcast does: one
capture, each extra rendition scaled from the captured frame to that fraction
of its pixels (no decode), with a bitrate derived from the scaled size, frame
rate, and codec, encoded only while someone watches it. Rust embedders get
the same through `moq_video::Capture`. Several microphones, captions from
capture, and changes to `moq transcode` are out of scope.

## Plan

Asked 2026-11-18 by the maintainer, after
[moq-dev/moq#7945](https://github.com/moq-dev/moq/issues/7945#issuecomment-6975233746)
wanted a command-line publisher that can do what the web publisher's
simulcast does. Today capture publishes one rendition
(`rs/moq-cli/src/publish.rs` `CaptureArgs`, `rs/moq-video/src/encode/capture.rs`).
`moq transcode` can already add a ladder beside it, but it decodes the
published stream again and publishes under `cam.hang/transcode.hang`.

What already exists: `encode::Producer`'s `Options::lower`
(`rs/moq-video/src/encode/producer.rs`) chains simulcast renditions, funding
the lower one first and disabling a higher one while the grant can't fund it,
and moq-ffi and libmoq expose it; `Frame::resize` scales a frame
(`rs/moq-video/src/frame.rs`); js/publish sizes a rendition with `maxScale`
(`js/publish/src/video/encoder.ts`).

Decided 2026-11-18 (maintainer, /quest-import):

- ✅ Goal as above (rejected: also capture several microphones as audio
  renditions; only document running `moq transcode` beside capture).
- ✅ Flag `--rendition NAME=SCALE`, repeatable: `SCALE` is the fraction of
  the captured pixels, the same meaning as JS `maxScale`, and the rendition
  is `video/NAME` (rejected: transcode's `--rung <short side>:<bps>`, since a
  fixed size and bitrate assume a source size capture doesn't know; a `--sd`
  preset; both).
- ✅ Opt-in: no extra rendition without the flag (rejected: a default SD
  rendition, which changes every capture's catalog and spends encoder
  sessions).
- ✅ Renditions chain through `Options::lower` in scale order, all on the
  connection's allocator, so under congestion SD keeps playing and the larger
  ones disable (rejected: each backing off independently, so every rendition
  degrades together).
- ✅ API: a renditions field on `moq_video::Capture` (it is
  `#[non_exhaustive]`, so additive) and the CLI flag; bindings get it when a
  binding user asks (rejected: moq-ffi now; CLI only).

Recommended: refuse at startup a scale outside (0, 1), a name that repeats,
and a name containing `/`; derive each bitrate with the formula the main
rendition already uses; and keep `--video demand` meaning the capture closes
when no rendition is watched. Exercise it with a real camera or
`--display` against a relay, watching `video/sd` while `video` is unwatched
and under an impaired uplink.

Public API: additive `moq_video::Capture` field and `--rendition` flag.
Wire: none; the catalog gains renditions.

## Related

- [An unwatched video rendition rests](/quest/m3/video-unwatched-rest.md) - changes how a rendition with no `lower` encodes while unwatched
