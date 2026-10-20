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
- Derive `fov` from the equirectangular bounds.
- Refuse what the catalog cannot describe, since an absent field claims flat
  video or plain channels: a cubemap or mesh projection, so Google's
  mesh-based VR180 is refused until the catalog has a mesh kind. For Opus
  family 2, RFC 8486 allows `(1 + n)^2 + 2j` channels with `j` 0 or 1; only
  `j = 0` with `n` of at least 1 (4, 9, 16, ... channels) maps to
  `ambisonic`, and zeroth order or a non-diegetic stereo pair (1, 3, 6, 11,
  ... channels) is refused. A count RFC 8486 forbids is malformed and refused
  the same way.
- `mp4-atom` 0.16.2 has no `st3d` or `sv3d` atom; add them in
  kixelated/mp4-atom. `webm-iterable` already names the Matroska elements.
- An fMP4 Opus track needs the `dOps` mapping table that
  [CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) adds, so ambisonic
  fMP4 follows it; Matroska and raw Opus do not wait.
- Exporters writing the fields back are out of scope.

Tests: a fixture per source (MP4 360, a 180-degree equirectangular MP4 with
`equi` bounds, Matroska stereo, Opus first-order ambisonics) imports with the
expected fields, and a cubemap, a mesh VR180, an ambisonic stream with
non-diegetic stereo, and a malformed one are refused.

Public API: none beyond the catalog fields #5543 adds. Wire: none.

## Related

- [WebXR playback](/quest/m3/watch-webxr.md) - `<moq-watch>` renders these fields
- [CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) - the `dOps` mapping table an ambisonic fMP4 track needs
