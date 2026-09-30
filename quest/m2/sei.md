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

## Related

- [Colour model](/quest/m1/color-model.md) - preserves display metadata semantics
