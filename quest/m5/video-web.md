# [L] moq-video in the browser

## Goal

moq-video runs in the browser for the
[web studio](https://github.com/moq-dev/studio/blob/main/quest/m2/web.md) in moq-dev/studio:
- **Capture:** getUserMedia for cameras and getDisplayMedia for screens,
  producing `Frame`s backed by WebCodecs `VideoFrame`s.
- **Codecs:** encode and decode through WebCodecs for whichever of H.264,
  HEVC, AV1, and VP9 the browser supports.
- **GPU import:** a `VideoFrame` becomes a wgpu texture with no CPU readback.

`hang` and `moq-mux` also build for `wasm32-unknown-unknown`. Their tokio
`fs` feature and file IO sit behind a native-only cfg, the prerequisite for
browser media muxing. A CI `cargo check` for wasm32 keeps them building. moq
removed its WASM support on 2026-10-31, so this quest restores the wasm32
target.

A test in a headless browser proves capture from a fake device, an encode and
decode round trip, and the import, so this lands without the studio page.

## Plan

Decided 2026-11-09 (maintainer, moq-dev/studio /quest-audit), answering
whether the web studio still wants this: the studio's web build moved from
its m0 to its m2, so it is still planned and this stays deferred in m5. The
studio waits on it through its
[browser video pin quest](https://github.com/moq-dev/studio/blob/main/quest/m2/pin-browser-video.md).

Changed 2026-11-02 (maintainer): the studio moved to moq-dev/studio. This
was planned to land in the same PR as the first studio page; it now lands
here first with its own browser test, and the studio's page follows a pin
bump.

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
