# [L] moq-video in the browser

## Goal

On wasm32, moq-video gains browser backends:
- **Capture:** getUserMedia for cameras and getDisplayMedia for screens,
  producing `Frame`s backed by WebCodecs `VideoFrame`s.
- **Codecs:** encode and decode through WebCodecs for whichever of H.264,
  HEVC, AV1, and VP9 the browser supports.
- **GPU import:** a `VideoFrame` becomes a wgpu texture with no CPU readback.

The existing `capture::open`, `encode::Encoder`, and `decode` entry points
work unchanged on the web.

## Plan

Codec support is probed at runtime. An unsupported codec fails loudly rather
than silently picking another. Replacing js/publish's or js/watch's internals
with these backends is out of scope.

## Required

- [hang and moq-mux on wasm](/quest/m3/studio/web/wasm-mux.md) - moq-video's media dependencies build for wasm32
- [A runtime trait replaces web-async](/quest/m2/runtime-trait.md) - spawn and time on the browser instead of tokio's runtime
