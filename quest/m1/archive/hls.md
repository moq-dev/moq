# [L] Offline archive HLS

## Goal

`moq-hls` serves ordinary HLS from a growing or static archive without a second
stored media copy, and renders playlists without downloading media objects.

## Plan

Use the archive catalog for rendition and initialization metadata, then reuse
the live HLS renderer against the timeline. Segment number, PTS, duration,
track ranges, gaps, and keyframe state provide everything needed for a media
playlist. Playlist generation and reloads read the timeline only; a regression
test must fail if they GET a media segment.

When a player requests media, use the recording reader to GET only the selected
range-named object and transmux its groups on demand. Switching between
360p and 1080p must not download both rendition objects. Emit media URIs with
the track and inclusive group bounds from the timeline record, so the handler
can derive `groups/<largest>.<smallest>` directly without listing or a
segment-ID lookup. HLS sequence numbers do not appear in storage keys. Keep
one storage object per track per timeline segment; a MoQ group need not be an
HLS segment, especially for one-group-per-frame audio. No LL-HLS parts.

Emit `EXT-X-ENDLIST` exactly when the timeline track the exporter reads
finishes cleanly, as the live export already does
(`rs/moq-hls/src/export/mod.rs:325-327`, `rendition.rs:243-246`). The store
holds no completion marker: the reader finishes the replayed timeline track
when its caller supplies finality out of band, so a standalone or BYOB archive
without such a caller stays a reloadable playlist.

Initially select the historic catalog snapshot by the existing moq-net timestamp
rule. Explicit group-to-catalog identity belongs to the related
[Catalog version binding](/quest/m1/archive/catalog-version.md) quest.

Prove aligned audio/video switching, missing track segments, discontinuities,
catalog changes, caller-supplied finality, and bounded LRU reads.

## Required

- [Recording reader](/quest/m1/archive/reader.md)
