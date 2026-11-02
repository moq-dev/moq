# [L] moq-video in the browser

## Goal

The first `/studio` page in demo/web: the wasm studio shows camera and screen
sources on one canvas and publishes its video to a relay. That page is the
consumer, so it lands in the same PR as moq-video's browser backends:
- **Capture:** getUserMedia for cameras and getDisplayMedia for screens,
  producing `Frame`s backed by WebCodecs `VideoFrame`s.
- **Codecs:** encode and decode through WebCodecs for whichever of H.264,
  HEVC, AV1, and VP9 the browser supports.
- **GPU import:** a `VideoFrame` becomes a wgpu texture with no CPU readback.

`hang` and `moq-mux` also build for `wasm32-unknown-unknown`. Their tokio
`fs` feature and file IO sit behind a native-only cfg, the prerequisite for
browser media muxing. A CI `cargo check` for wasm32 keeps them building. moq
removed its WASM support on 2026-10-31, so this quest restores the wasm32
target; confirm the web studio still wants it before starting.

## Plan

- **Asynchronous seams:** the encode and decode seams become asynchronous.
  Today the backend's `encode` and `flush` return their packets
  synchronously, but WebCodecs delivers output through a callback and drains
  through a promise. The new seam submits frames, yields packets as they
  arrive, and flushes with backpressure.
- **Runtime:** browser objects are not `Send`, so these backends run on the
  browser runtime.
- **API changes:** any change to a published entry point says how to
  migrate in the PR's Impact bullet.
- **Codec support** is probed at runtime. An unsupported codec fails loudly
  rather than silently picking another.
- **Out of scope:** replacing js/publish's or js/watch's internals with these
  backends.

## Required

- [Libraries spawn nothing](/quest/m1/poll-first/no-spawn.md) - library work polled by its owner, with no runtime to spawn on
