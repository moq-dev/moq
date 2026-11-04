# [S] The WebGPU renderer is verified on Safari 26

## Goal

`@moq/video`'s WebGPU path is verified on Safari 26, on macOS and iOS: H.264
and biplanar frames import through `importExternalTexture`, orientation is
right, and a frame that fails to import is handled as decided below, with
evidence.

## Plan

Planned 2026-10-10 as a follow-up of
[moq-dev/moq#5138](https://github.com/moq-dev/moq/pull/5138), which shipped the
renderer without Safari verification. The original concerns were H.264
frames, biplanar formats, and iOS orientation with `importExternalTexture`.

- Run `<moq-watch>` and the `<moq-publish>` preview with `backend` set to
  `auto` and `webgpu` on real Apple hardware or a hosted Safari session, over
  H.264 and the other catalog codecs, in each orientation.
- [moq-dev/moq#5138](https://github.com/moq-dev/moq/pull/5138) decided an
  unimportable frame stays loud (it throws and that frame is
  skipped). Revisit with what Safari actually does: keep it, or report it
  through `out.error` under its own error name. No valid macOS frame failed to
  import in the run below, so this decision remains unchanged.
- Fix what breaks at its source. If the probe passes but frames fail, consider
  extending the probe rather than a runtime fallback, which that PR rejected.

Verification on 2026-11-03 used physical Apple M4 hardware (Mac16,13), macOS
26.6.2 build 25G83, and Safari 26.6.2 through its existing WebDriver setup.
The source was `976fece17349666bb2ab6dad93109408ac272e8d`. The adapter reported
`vendor: apple` and `isFallbackAdapter: false`; neither was overridden.
Pixel readback checked four colored quadrants, with no renderer changes.

- Direct WebGPU rendering passed 80/80 cases: RGBA, NV12, I420, H.264 High,
  Main and Baseline, HEVC, VP8, VP9 and AV1, each rotated 0/90/180/270 degrees
  and mirrored or unmirrored. Decoded frames were NV12. VP9 and AV1 used
  independent 640x360 FFmpeg fixtures; Safari's own encoders failed, and its
  decoders also refused the earlier 64x32 fixtures.
- `<moq-watch>` passed 112/112 cases: those seven encoded codec/profile
  variants, both `auto` and `webgpu`, all four catalog rotations and both
  mirror settings. Broadcasts used a page-local origin, so this is rendering
  evidence, not transport interoperability evidence.
- `<moq-publish>` passed 10/12 landscape image-source preview cases, covering
  `source` and `encoded` with both backends for H.264 High, HEVC and VP8. Both
  HEVC `encoded` cases failed before rendering with
  `preview: decode error EncodingError: Decoder failure`: Safari accepts
  `hevc: { format: "annexb" }` but returns length-prefixed chunks (prefix
  `00 00 00 3b 4e 01`) with a 109-byte hvcC description.

✅ Safari HEVC: probe and refuse (maintainer, 2026-11-03, on
[moq-dev/moq#7765](https://github.com/moq-dev/moq/pull/7765)). The encoder
setup probe in `@moq/publish` now checks that H.264 and HEVC output opens with
an Annex B start code and otherwise skips that codec, so publishing and the
encoded preview pick the next one. No runtime fallback. On the same Safari
26.6.2 Mac, the probe refused `hev1.1.6.L93.B0` and resolved `avc1.640028`,
whose output was Annex B.

Remaining evidence includes real iOS capture and physical orientation,
publisher preview orientation, and a valid frame that actually fails import.
A deliberately closed RGBA frame did not induce an import exception or
validation error in the macOS session; it does not verify failure handling.
Keep the renderer's existing error behavior pending such evidence.

Public API: possibly a new `out.error` name. Wire: none.

## Waiting

- Maintainer: run Safari 26 on a physical iPhone or iPad (a simulator does not
  count), with Web Inspector attached from a Mac. Serve the `main` demo
  (`just demo`) to the device over HTTPS, since camera capture needs a secure
  context. Then:
  1. Open `publish.html`, publish the rear camera, then the front camera. In
     the console, run `document.querySelector("moq-publish").backend = "webgpu"`,
     then `"auto"`. For each, rotate the device through portrait, landscape
     left, landscape right, and upside down. Check that the preview stays
     upright and matches the scene.
  2. Open `watch.html` on the same broadcast and set
     `document.querySelector("moq-watch").backend` to `"webgpu"`, then `"auto"`.
     Repeat the rotations and check that the picture stays upright.
  3. In the console, run `(await navigator.gpu.requestAdapter()).info`. Record
     its result, the iOS and Safari versions, any `moq-publish: skipping`
     warning, and any `importExternalTexture` or WebGPU validation error in
     this quest.

  Done when those results are recorded here, so the remaining work can be
  fixed at its source or the quest finished.

## Related

- [WebGPU HDR](/quest/m2/webgpu-hdr.md) - builds on the same renderer
