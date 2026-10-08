# [L] Video renders through WebGPU

## Goal

A new `@moq/video` package renders video frames through WebGPU where the
browser has it, importing each `VideoFrame` with `importExternalTexture`
(zero copy where the browser allows), in the frame's own colour space
(sRGB or Display P3). Where WebGPU or a `VideoFrame` external texture is
missing, it renders through the existing Canvas2D path. `@moq/watch` and the
`@moq/publish` preview both use it, so the preview gains rotation too.

Not here: HDR output, which is [WebGPU HDR](/quest/m1/webgpu-hdr.md); moving
playback into a worker, which is [watch worker](/quest/m1/watch-worker.md);
and moving the WebCodecs decoder or encoder out of watch and publish.

## Plan

Decided 2026-10-08 in a `/quest-plan` interview (paper trail in the PR that
added this quest). Re-planned from issue #703, whose stub quest the
2026-09-28 audit (#4393) dropped:

- Why: correct colour (and later HDR), and draw cost. Measured on a 1080p
  microbenchmark (webcodecsfundamentals.org), Canvas2D vs WebGPU frames per
  second: Firefox 70 vs 430, Safari 230 vs 610, Chrome 960 vs 1230. Rejected
  as reasons on their own: an effects hook, and replacing Canvas2D outright.
- Coverage (2026-10): WebGPU is Baseline, but Firefox on Linux, Intel Mac,
  and Android, and Chrome on Linux with GPUs other than Intel Gen12+ or NVIDIA
  on Wayland, still lack it, and `importExternalTexture(VideoFrame)` is
  missing on Firefox Android. So Canvas2D stays as an explicit second
  renderer. Rejected: WebGPU only, refusing elsewhere.
- Package: a new `@moq/video`, mirroring Rust's moq-video, holding only the
  renderer for now (the WebGPU and Canvas2D paths, the presentation
  transform from `js/watch/src/video/presentation.ts`, and selection).
  `@moq/watch` and `@moq/publish` depend on it. Rejected: putting it in
  `@moq/hang` (mixes a DOM and GPU concern into the media layer), and making
  publish depend on watch. Adds the release scaffolding and a
  `doc/lib/js/video.md` page.
- A public `renderer` option, `"auto" | "webgpu" | "2d"`: an attribute on
  `<moq-watch>` and `<moq-publish>` and an input on the renderer. `"auto"` (the
  default) picks WebGPU where supported, else Canvas2D, and logs the choice.
  An explicit `"webgpu"` where it is missing refuses loudly; only `"auto"`
  falls back.
- Device loss: request a new device and rebuild the pipeline. If no adapter
  comes back, `"auto"` switches to Canvas2D (logged) rather than going black.
  A canvas that acquired a `webgpu` context never returns a `2d` one, even
  after device loss, and the canvas is caller-owned (the `<canvas>` child of
  `<moq-watch>` or `<moq-publish>`, or a transferred `OffscreenCanvas`). Open:
  who supplies the replacement surface for this fallback, including in the
  worker. Settle it with the maintainer before building the fallback.
- The renderer takes an `HTMLCanvasElement` or an `OffscreenCanvas`, so the
  [watch worker](/quest/m1/watch-worker.md) move transfers the canvas and
  carries this renderer over unchanged. This lands first, on the main thread,
  behind the existing `Renderer` API.
- Keep today's behaviour: rotation and flip parity with
  `canvasPresentationTransform`, the canvas sized to the video's display size
  (no devicePixelRatio scaling), `out.frame`, `out.timestamp`, and
  `out.visible`. Import, bind, encode, and submit each frame with no `await`
  in between, since the external texture expires when the frame closes.
  Adapter and device setup is async, so the first paint waits on it.
- Watch for known Safari 26 issues with `importExternalTexture` (H.264
  frames, biplanar formats, iOS orientation) and test on Safari.
- Measurement: ship, then measure. Extending the browser benchmarks with a
  per-browser Canvas2D vs WebGPU comparison is a follow-up, not a gate.
- Tests: selection (auto picks WebGPU or Canvas2D by support, an explicit
  `"webgpu"` refuses where missing), rotation and flip parity between the two
  paths, device-loss recovery, and the preview's rotation. The fallback
  test runs in a real browser: paint with WebGPU, lose the device with no
  adapter to recover, then check Canvas2D actually paints.

Public API: new `@moq/video` package; a `renderer` attribute and option on
`<moq-watch>` and `<moq-publish>`; the publish preview's renderer moves to
the shared one. Wire: none.

## Closes

- [#703](https://github.com/moq-dev/moq/issues/703) - close this issue when the quest finishes

## Related

- [WebGPU HDR](/quest/m1/webgpu-hdr.md) - HDR output on top of this renderer
- [Watch worker](/quest/m1/watch-worker.md) - moves playback, and this renderer, into a worker
- [Catalog colour](/quest/m1/color-catalog.md) - colour metadata the HDR follow-up reads
