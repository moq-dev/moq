# [S] moq-mux importers fill the VR catalog fields

## Goal

The `moq-mux` importers fill the hang catalog's spatial fields from
[#5543](https://github.com/moq-dev/moq/pull/5543): a video rendition's
`projection` and `stereo` from MP4 `st3d`/`sv3d` (Spherical Video V2) and
Matroska `Projection`/`StereoMode`, and an audio rendition's
`layout: "ambisonic"` from Opus channel mapping family 2. Media without these
boxes or elements imports as today.

## Plan

Decided with the maintainer on 2026-10-20, from #5543's follow-ups. Start once
#5543 lands.

- The fields are live state like `color` (#5543), so fill them the way the
  importers fill color (`rs/moq-mux/src/codec/color.rs`) and update them when
  the source's config changes.
- Derive `fov` from the equirectangular bounds. A projection the catalog has
  no kind for (cubemap, mesh) leaves the field absent: a flat picture is a
  worse result, not a wrong one. Malformed input, such as family 2 with a
  channel count that is not a square of at least 4, is refused.
- `mp4-atom` 0.16.2 has no `st3d` or `sv3d` atom; add them in
  kixelated/mp4-atom. `webm-iterable` already names the Matroska elements.
- An fMP4 Opus track needs the `dOps` mapping table that
  [CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) adds, so ambisonic
  fMP4 follows it; Matroska and raw Opus do not wait.
- Exporters writing the fields back are out of scope.

Tests: a fixture per source (MP4 360 and VR180, Matroska stereo, Opus
first-order ambisonics) imports with the expected fields, and a malformed one
is refused.

Public API: none beyond the catalog fields #5543 adds. Wire: none.

## Related

- [WebXR playback](/quest/m3/watch-webxr.md) - `<moq-watch>` renders these fields
- [CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) - the `dOps` mapping table an ambisonic fMP4 track needs
