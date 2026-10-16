# [S] Every video importer reports the display aspect

## Goal

A video rendition with non-square samples gets the catalog display aspect
from its bitstream, whichever importer reads it:

- H.264 (the VUI sample aspect)
- H.265 from both Annex-B and an `hvcC` record
- AV1, if it carries a stream-level signal

A square-sample stream still leaves the fields unset, as the encode hint does
today.

## Plan

[#5574](https://github.com/moq-dev/moq/pull/5574) fixed the H.265 Annex-B path
(`config_from_sps`, `rs/moq-mux/src/codec/h265/import.rs`). The remaining
gaps:

- `config_from_hvcc` sets only the coded size and description. Share the
  Annex-B path's VUI read rather than duplicating it.
- The H.264 SPS parser skips `sar_width`/`sar_height`.
- AV1 has no sample aspect in its sequence header. Check whether
  `render_size` should count as a display aspect before adding it. If not,
  record why and leave AV1 out.
- `MoqVideo.display_aspect` (and `coded`) in `rs/moq-ffi/src/media.rs` have
  no doc line.

Keep #5574's choice that an encode hint fills in only square samples.

Tests: a non-square fixture per codec path, and a square one that stays
unset.

Not planned: an upstream scuffle_h265 issue for the unchecked arithmetic in
`cropped_width()`/`cropped_height()`. We never call them, since our SPS
reader crops with checked math.

Public API: doc only. Wire: the catalog's existing display aspect fields are
populated in more cases.

## Related

- [Catalog colour](/quest/m2/color-catalog.md) - parses the same H.264/H.265 VUI for colour
