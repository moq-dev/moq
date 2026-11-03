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
  `preview: decode error EncodingError: Decoder failure`.
- The HEVC reproduction requested `hevc: { format: "annexb" }`, but Safari
  returned a length-prefixed chunk (prefix `00 00 00 3b 4e 01`) and an hvcC
  decoder description. `preview.ts` configures its decoder without that
  description. The publishing probe also assumes the requested Annex B
  format was honored, so a preview-only decoder change would leave the
  publishing path unresolved.

Remaining evidence includes real iOS capture and physical orientation,
publisher preview orientation, and a valid frame that actually fails import.
A deliberately closed RGBA frame did not induce an import exception or
validation error in this Safari session; it does not verify failure handling.
Keep the renderer's existing error behavior pending such evidence.

Public API: possibly a new `out.error` name. Wire: none.

## Waiting

- Maintainer: connect real iOS hardware running Safari 26 to an interactive
  or hosted session. The available device tools expose only a Linux host and
  no iOS devices; SSH/WebDriver supplied macOS evidence only. Recommend a
  physical iPhone or iPad session that permits capture and rotation. A
  simulator does not clear this wait. Resume the remaining matrix once that
  session is reachable.
- Maintainer: decide how to handle Safari ignoring HEVC Annex B encoding.
  Recommend verifying the output format in the existing encoder setup probe
  and refusing HEVC when unsupported, covering both publishing and encoded
  preview. The alternative is supporting Safari's length-prefixed output
  while preserving the publisher's inline parameter sets and adaptive
  resizing. This changes supported publishing behavior, so the verification
  attempt leaves the choice open rather than adding a runtime fallback.

## Related

- [WebGPU HDR](/quest/m2/webgpu-hdr.md) - builds on the same renderer
