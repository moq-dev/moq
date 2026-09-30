# [M] Encoders signal the colour they produce

## Goal

Every moq-video encode path signals the colour its output pixels actually
have. The CUDA, D3D, and Android paths preserve primaries, transfer, matrix,
and range through conversion, and a path that cannot honour the requested
colour converts correctly or refuses instead of mislabelling.

## Plan

Use the settled main frame contract and the existing extensible Color metadata;
do not introduce a second native-frame hierarchy. Fix encoding paths that
warn about a known color mismatch and then label unchanged pixels as the
requested color: convert correctly or refuse. Preserve color information
through CUDA, D3D, and Android paths and test the signaled VUI against pixels.

Decided in the 2026-09-30 audit: split from the catalog colour model, which
moved to [Catalog colour](/quest/m2/color-catalog.md) because no renderer
consumes colour from the catalog today. Encoder correctness stays in m1
because a mislabelled stream is wrong for every consumer.

## Related

- [Catalog colour](/quest/m2/color-catalog.md) - the catalog describes a rendition's colour and HDR properties
- [SEI sidecars](/quest/m2/sei.md) - moves SEI out of the video track;
  the display metadata inside it needs the home this quest builds
