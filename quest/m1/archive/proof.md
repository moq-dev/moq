# [M] Archive proof

## Goal

Prove deterministic segment storage, exact FETCH replay, selective rendition
reads, and timeline-only HLS generation from one multi-rendition broadcast.

## Plan

Record explicitly enrolled video, audio, catalog, and non-media tracks. Cut each
track on its own timeline, including many audio groups per object and a group
split across objects by frame, then
replay their original sequences, timestamps, and payloads through
`track::Dynamic`.

Verify the exact object keys and bytes on memory, local, and S3-compatible
`object_store` implementations, including the percent-encoded track names. A
360p or audio-only FETCH must not GET the 1080p object, while adjacent group
requests should hit the segment LRU.

Cover the persistence boundary: a crash after segment PUT but before timeline
commit leaves invisible orphan data that a listing bootstrap ignores; a failed
or mismatched `.info` exposes no ranges; a failed independent track PUT omits
only that track while the record's other tracks stay advertised; later
segments remain usable. Catalog-to-group applicability is outside this proof. Also
cover a stalled pacing track forced to a gap, sparse group ranges, malformed
table offsets, an unknown envelope or `.info` version treated as a missing
segment, segment create collisions under the single-writer rule, a missing
tail, and a clean end without a completion marker. Accept equivalent `.info`
JSON with reordered members or different whitespace without rewriting it;
reject differing properties and malformed or unsupported metadata.

Exercise group and segment IDs 0 and 2^53 - 1; reject 2^53 and the largest
QUIC varint in keys and reconstructed group IDs. Cover consecutive zero
deltas, sparse deltas, overflow, stopping at ID exhaustion without wrapping
or inferring clean finality, empty objects, mismatched filename bounds, and overlapping
ranges across segments. Reject decreasing or duplicate group arrivals while
allowing accepted groups to complete out of order. Verify JSON-safe timescales
and timestamps, accepting timestamp 2^53 - 1 and rejecting 2^53.
Test ordered S3 lookup at both endpoints and between ranges, unordered paginated
results, incremental cursor recovery, DVR expiration while a reader is offline,
and stale media listings preceding a new timeline commit. Following N+1 must
not refresh all media listings. Wire these cases into CI for the store, writer,
and reader implementations; do not add an unconnected standalone proof script.

The store's own tests also cover recording-prefix isolation (`rec` beside
`rec-other`), empty prefixes, track-prefix listings, continuation pages, and
every supported pagination option. A page must not lose directory entries
silently or fail because the backend matched a neighbouring recording. Every
publicly constructible key either serializes to a path its parser accepts or
fails before storage; direct `Key::Groups` construction must not bypass range
validation. These cases belong beside the store and codec code and need no
new public API or format change.

Crash a DVR writer after its pop becomes durable but before media deletion.
On exclusive restart, prove that expired and uncommitted group objects are
removed after the grace period while retained media, `.info`, and checkpoint
objects survive. Failed or incomplete recovery/listing must delete nothing;
restart must finish this cleanup before accepting new groups.

Finally render and reload HLS playlists while rejecting every media-object GET
until a segment URI is requested. A segment request must resolve one object
from the replayed timeline without any listing or separate index object.

## Required

- [Rust per-track timelines](/quest/m1/archive/track-timeline/core.md) - proves the per-track format, not the aligned one
