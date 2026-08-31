# [M] Archive proof

## Goal

Prove deterministic segment storage, exact FETCH replay, selective rendition
reads, and timeline-only HLS generation from one multi-rendition broadcast.

## Plan

Record explicitly enrolled video, audio, catalog, and non-media tracks. Cut at
aligned keyframe boundaries, including multiple audio groups per segment, then
replay their original sequences, timestamps, and payloads through
`track::Dynamic`.

Verify the exact object keys and bytes on memory, local, and S3-compatible
`object_store` implementations. A 360p or audio-only FETCH must not GET the
1080p object, while adjacent group requests should hit the segment LRU.

Cover the persistence boundary: a crash after segment PUT but before timeline
commit leaves invisible orphan data; a failed or mismatched `.info` exposes no
ranges; a failed independent track PUT omits only that track; a failed catalog
PUT never exposes dependent media; later segments remain usable. Also cover a
stalled pacing track forced to a gap, sparse group ranges, malformed offsets,
unknown format versions, segment create collisions under the single-writer
epoch invariant, a missing tail, and clean end without a completion marker.

Finally render and reload HLS playlists while rejecting every media-object GET
until a segment URI is requested.

## Required

- [Recording writer](/quest/m1/archive/writer.md)
- [Offline archive HLS](/quest/m1/archive/hls.md)
