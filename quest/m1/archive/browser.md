# [L] Browser archive

## Goal

Browser-published broadcasts implement the same archive timeline, segment
layout, and FETCH behavior as native `moq-archive` users.

## Plan

Port the archive contract to the JS packages with memory and OPFS storage. The
application explicitly enrolls video, audio, catalog, or arbitrary data tracks;
the archive does not infer them from Hang. Use the shared timeline segmenter and
merged JSON Window, including application-driven keyframe cuts.

Persist one object per `(track, segment)` after all included groups complete,
then publish the archive timeline record. A typical audio segment contains many
one-group-per-frame audio groups. Match the Rust binary envelope and track-info
vectors byte for byte. Match the writer's generic commit prerequisites too, so
a failed catalog snapshot never leaves dependent media ranges advertised.

Add the JS equivalent of `track::Dynamic` so the publisher can answer FETCH
misses from memory or OPFS after relay eviction. Keep the bounded bytes in an
LRU and use the same timeline-before-delete ordering for DVR retention.

Ship the contract in the `@moq/*` packages. A dashboard browser-to-HLS proof
remains downstream (moq.pro) work.

## Required

- [Archive catalog](/quest/m1/archive/catalog.md)
- [Recording writer](/quest/m1/archive/writer.md)
- [Archive store](/quest/m1/archive/store.md)
- [Archive timeline](/quest/m1/archive/timeline.md)
