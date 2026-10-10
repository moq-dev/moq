# [S] The WebGPU renderer is verified on Safari 26

## Goal

`@moq/video`'s WebGPU path is verified on Safari 26, on macOS and iOS: H.264
and biplanar frames import through `importExternalTexture`, orientation is
right, and a frame that fails to import is handled as decided below, with
evidence.

## Plan

Planned 2026-10-10 as a follow-up of #5138, which ships the renderer
untested on Safari. Known Safari 26 issues with `importExternalTexture`: H.264
frames, biplanar formats, and iOS orientation.

- Run `<moq-watch>` and the `<moq-publish>` preview with `backend` set to
  `auto` and `webgpu` on real Apple hardware or a hosted Safari session, over
  H.264 and the other catalog codecs, in each orientation.
- #5138 decided an unimportable frame stays loud (it throws and that frame is
  skipped). Revisit with what Safari actually does: keep it, or add a
  `RendererError` value for it.
- Fix what breaks at its source. If the probe passes but frames fail, consider
  extending the probe rather than a runtime fallback, which #5138 rejected.

Needs a human-run or hosted Safari session; a background agent reports
instead of guessing.

Public API: possibly a new `RendererError` value. Wire: none.

## Related

- [WebGPU HDR](/quest/m1/webgpu-hdr.md) - builds on the same renderer
