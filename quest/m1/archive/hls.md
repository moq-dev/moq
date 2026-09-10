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

Use the catalog supplied to the exporter. This quest does not establish which
catalog update applies to a historic group; timestamps do not provide an
explicit binding. Track immutability and update correlation belong to
[Catalog track identity](/quest/m2/catalog-tracks.md), independently of DVR.

Prove aligned audio/video switching, missing track segments, discontinuities,
caller-supplied finality, and bounded LRU reads.

## Required

- [Recording reader](/quest/m1/archive/reader.md)
