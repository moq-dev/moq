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
[Catalog track identity](/quest/m3/catalog-tracks.md) addresses that separately.

Use [JavaScript FETCH](/quest/m2/js-fetch.md)'s on-demand group requests to
answer cache misses from memory or OPFS after relay eviction. This quest owns
storage lookup, the bounded segment LRU, and timeline-before-delete DVR
retention; the generic request lifecycle and IETF wire support land in the
prerequisite. Verify browser archive replay against native subscribers across
the supported drafts without duplicating transport dispatch or codecs here.

Ship the contract in the `@moq/*` packages. A dashboard browser-to-HLS proof
remains downstream (moq.pro) work.

## Required

- [JavaScript FETCH](/quest/m2/js-fetch.md) - generic on-demand group serving and IETF FETCH support

- [Recording writer](/quest/m2/archive/writer.md)
