# SEI separation evaluation

## Goal

Keep H.264 and HEVC SEI inline unless measured savings or a concrete
metadata-only consumer justify separate delivery. Evaluate that tradeoff before
committing a catalog, marker, or reassembly format.

## Plan

SEI separation is deferred. Existing metadata consumers may inspect the inline
bitstream; this line does not block captions or unrelated timed-metadata
carriage. The schema and implementation quests are conditional follow-ons,
not permission to strip by default. A no-go verdict abandons them.

Preserve decoder, display, recovery-point, and caption behavior. Raw vendor
payloads may be large, but that does not establish typical bandwidth savings.
Total storage and bytes delivered to a video-only subscriber are different
measurements. Any future split must state which payloads move and how missing
metadata is handled; do not promise byte-faithful export after a deadline miss.

## Quests

- [SEI evidence](/quest/future/sei/evidence.md) - measure savings and identify a consumer before deciding whether to split
- [SEI section](/quest/future/sei/sei.md) - define a format only after a positive verdict and settled association policy
- [Rust split and reinsert](/quest/future/sei/sei-rust.md) - implement the approved split and bounded export behavior
- [Web access](/quest/future/sei/sei-web.md) - expose approved sidecar samples independently of video

## Related

- [Metadata association](/quest/next/metadata-association.md) - independently plans carriage for metadata already outside video
- [Colour model](/quest/next/color-model.md) - preserves display metadata semantics
- [CEA-608/708](/quest/next/captions-cea.md) - can read inline caption SEI without waiting for this experiment
