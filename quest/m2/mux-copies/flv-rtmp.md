# [S] Measure and reduce FLV tag-body copies

## Goal

Reduce measured FLV tag-body and media-payload copying without changing codecs,
metadata, timestamps, published media, public APIs, or wire formats.

## Plan

FLV import copies a tag body from its input buffer and then copies the selected
media payload again. Samples are already in their length-prefixed codec shape;
there is no Annex-B splitter to remove on this path.

Investigate detaching a complete tag and sharing its media range through private
parsing helpers. Preserve legacy and enhanced tags, multitrack routing,
configuration records, timestamp offsets, partial-input behavior, and existing
validation/errors. Measure retained backing-buffer memory when small samples
share a large input allocation.

Add Criterion import cases for representative legacy AVC/AAC and enhanced tags
with varied input chunk sizes. Wire payload/catalog/timing equivalence and
fragmented/malformed-tag fixtures into existing mux CI tests. Follow the
measurement and no-win completion rules in the
[questline](/quest/m2/mux-copies/README.md).

## Related

- [FLV script tags](/quest/m2/flv-script.md) - independent metadata work
- [RTMP chunk assembly](/quest/m2/mux-copies/rtmp.md) - independently shippable transport work
