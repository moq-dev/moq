# [M] HDR video renders as HDR

## Goal

An HDR rendition (PQ or HLG) plays as HDR in the browser where the display
and browser can show it: the `@moq/video` WebGPU renderer converts the frame
itself and presents through an `rgba16float` canvas with extended tone
mapping. Elsewhere it is tone-mapped to SDR deliberately, instead of being
drawn as if it were SDR as today.

## Plan

Decided 2026-10-08 in the WebGPU renderer `/quest-plan` interview: SDR and
wide gamut first in [WebGPU renderer](/quest/m1/webgpu-renderer.md), HDR here,
both in m1.

Facts (2026-10):

- HDR canvas output ships only on Chromium desktop:
  `configure({ format: "rgba16float", toneMapping: { mode: "extended" } })`
  since Chrome 129. Safari 26 accepts the configuration on macOS but does not
  display it extended; Firefox has none. Canvas2D has no HDR output.
- `importExternalTexture` and `copyExternalImageToTexture` carry no HDR
  headroom (gpuweb#5236 is open), so imported video is SDR. HDR needs the
  renderer to convert the frame itself: read the planes, apply the matrix,
  range, transfer (PQ or HLG), and primaries, and tone-map when the display
  has less headroom.
- `VideoFrame.colorSpace` gives primaries, transfer, matrix, and range
  (Chrome 94, Firefox 130, Safari 16.4). Mastering display and content light
  levels come from [Catalog colour](/quest/m1/color-catalog.md).

Settle while building: whether the HDR path gives up zero copy (planes via
`copyTo` and a custom conversion shader), and how the renderer detects
display headroom.

Tests: an HDR10 rendition renders extended on Chromium desktop, tone-maps to
SDR elsewhere, and an SDR rendition is unchanged.

Public API: none beyond the renderer. Wire: none.

## Required

- [WebGPU renderer](/quest/m1/webgpu-renderer.md) - the renderer and `@moq/video` package this extends
- [Catalog colour](/quest/m1/color-catalog.md) - the mastering display and content light levels
