# [L] Browser archive

## Goal

Browser-published broadcasts implement the same archive timeline, segment
layout, and FETCH behavior as native `moq-archive` users.

## Plan

Port the archive contract to the JS packages with memory and OPFS storage. The
application explicitly enrolls video, audio, catalog, or arbitrary data tracks;
the archive does not infer them from Hang. The JS timeline already publishes
through the Window (`js/hang/src/container/timeline.ts:134`, `:165`, backed by
`js/json/src/window/`) with pacing tracks and application-driven cuts; add the
deferred commit Rust has (`Producer::deferred`,
`rs/moq-mux/src/timeline.rs:917`).

Persist one range-named object per track per segment after its groups complete,
then publish the archive timeline record. Match the 19-digit group-bound keys,
ascending delta-encoded IDs, and sequential timeline discovery used by Rust. A typical audio segment contains many
one-group-per-frame audio groups. Match the Rust binary envelope bytes and `.info` property values, per the [Recording section](/drafts/draft-lcurley-moq-hang.md#recording), without inferring catalog-to-group applicability.
[Catalog track identity](/quest/m2/catalog-tracks.md) addresses that separately.

The missing piece in `js/net` is a `track::Dynamic` equivalent: a consumer can
`fetchGroup` (`js/net/src/track.ts:272`), but nothing in `js/net/src` lets a
producer serve that miss on demand. Add it so the publisher can answer FETCH
misses from memory or OPFS after relay eviction. Keep the bounded bytes in an
LRU and use the same timeline-before-delete ordering for DVR retention.

Ship the contract in the `@moq/*` packages. A dashboard browser-to-HLS proof
remains downstream (moq.pro) work.

## Required

- [Archive catalog](/quest/m2/archive/catalog.md)
- [Archive store](/quest/m2/archive/store.md)
- [Recording writer](/quest/m2/archive/writer.md)
