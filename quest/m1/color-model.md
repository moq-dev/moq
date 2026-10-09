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

Decided in the 2026-09-30 audit: split from the catalog colour model,
[Catalog colour](/quest/m1/color-catalog.md), which the WebGPU HDR renderer
now consumes (2026-10-08). Encoder correctness stays separate because a
mislabelled stream is wrong for every consumer.

## Related

- [Catalog colour](/quest/m1/color-catalog.md) - the catalog describes a rendition's colour and HDR properties
