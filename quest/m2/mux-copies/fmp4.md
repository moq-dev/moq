# [M] Cut fMP4 import and fragment-encode copies

## Goal

Coded bytes are copied once into the hang frame on import (zero extra if
the sliced `Bytes` can be the frame). `encode_fragment` allocates once for
the output fragment. Groups, `tfdt`, and catalog do not change.

## Plan

`container/fmp4/import.rs` `drain` fully decodes `Any::Mdat` (owned
`mdat.data`) and `consumed.slice` of the same bytes. `extract` then
`track_mdat_data.to_vec()` and re-encodes moof twice for `data_offset`.

`encode_fragment` flattens payloads with `flat_map(|f| f.payload.iter().copied())`,
encodes moof, clears, encodes again, and clones `TrunEntry`.

Parse moof only; treat mdat as a `Bytes` view. Rewrite offsets without
cloning sample bytes when the range is already contiguous. Size the moof
from `Trun` layout (or encode headers into a reserved prefix); write mdat
with `extend_from_slice`.

Acceptance: Criterion `fmp4.import` and `fmp4.encode_fragment` on
`test_data/bbb.mp4` and a multi-traf file; 1 / 30 / 180 samples. Count
copied payload bytes. No change in groups/`tfdt`/catalog.

## Related

- [CMAF copies](/quest/m2/cmaf-copy-budget.md) - JS encode/decode
- [CMAF hang groups passthrough](/quest/m2/mux-copies/cmaf-passthrough.md) - skip remux entirely on the HLS path
