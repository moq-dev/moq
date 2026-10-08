# [L] Browser archive

## Goal

Browser-published broadcasts implement the same archive timeline, segment
layout, and FETCH behavior as native `moq-archive` users.

## Plan

Port the archive contract to the JS packages with memory and OPFS storage. The
application explicitly enrolls video, audio, catalog, or arbitrary data tracks;
the archive does not infer them from Hang. Record against the per-track
timelines from [JS per-track timelines](/quest/m1/archive/js-timelines.md).

Commit each track independently: persist each stored span, including a frame
range of a still-open group, then publish that track's timeline record. Match
the object keys, envelope bytes, `.info` property values, and timeline
discovery the Rust writer uses, per the
[Recording section](/drafts/draft-lcurley-moq-hang.md#recording), without
inferring catalog-to-group applicability.
[Catalog track identity](/quest/m1/catalog-tracks.md) addresses that separately.

Use [JS ranges](/quest/m1/subscribe-ranges/js.md)'s on-demand range requests to
answer cache misses from memory or OPFS after relay eviction. This quest owns
storage lookup, the bounded segment LRU, and timeline-before-delete DVR
retention; the generic request lifecycle and IETF wire support land in the
prerequisite. Verify browser archive replay against native subscribers across
the supported drafts without duplicating transport dispatch or codecs here.

Ship the contract in the `@moq/*` packages. A dashboard browser-to-HLS proof
remains downstream (moq.pro) work.

Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no named consumer; the native archive
lands first.

## Required

- [JS ranges](/quest/m1/subscribe-ranges/js.md) - generic on-demand range serving in `@moq/net`
- [JavaScript FETCH](/quest/m1/js-fetch.md) - IETF FETCH dispatch onto it
- [JS per-track timelines](/quest/m1/archive/js-timelines.md) - the timeline this archive records against
- [Archive](/quest/m1/archive/README.md) - the native writer, reader, and DVR contract this ports
