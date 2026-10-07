# [S] HLS media sequence survives a reference switch

## Goal

A media playlist's `EXT-X-MEDIA-SEQUENCE` never decreases, as
[RFC 8216 §6.2.2](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.2.2)
requires, even when the reference rendition changes. Players keep loading the
same `media.m3u8` across the switch without stalling.

## Plan

Today `moq-hls` numbers segments by the reference rendition's own records. A
switch (a new first video rendition with a timeline, or the reference leaving
the catalog) starts a new numbering under a new URL tag
(`seg/{reference}.{segment}.m4s`, #4034). Its URLs never collide and a
discontinuity marks the switch, but the media sequence can rewind.

Open design, pick one:

- **Sticky reference.** Keep the reference while it stays in the catalog, so a
  new, earlier-named rendition no longer switches it. Cheap and edge-consistent,
  but a reference that leaves the catalog still rewinds.
- **Offset at the switch.** Number the new reference's segments from past the
  last listed one. Monotonic, but an edge that starts after the switch numbers
  differently, so edges disagree on the media sequence.

Recommendation: the sticky reference first. It removes the common trigger
without giving up the property that every edge agrees on numbering. Measure
whether a reference leaving the catalog happens in practice before taking on
the offset.

Test a switch through repeated playlist reloads: the media sequence never
decreases, and no URL names other content.

## Related

- [Fixed HLS target duration](/quest/m1/archive/hls-target.md) - also changes how the reference shapes the playlist
