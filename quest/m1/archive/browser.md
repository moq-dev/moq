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

Persist one object per `(track, segment)` after all included groups complete,
then publish the archive timeline record. A typical audio segment contains many
one-group-per-frame audio groups. Match the Rust binary envelope and `.info`
bytes exactly, per [format](/quest/m1/archive/format.md), and share the
writer's commit prerequisites so a failed catalog snapshot never leaves
dependent media ranges advertised.

The missing piece in `js/net` is a `track::Dynamic` equivalent: a consumer can
`fetchGroup` (`js/net/src/track.ts:272`), but nothing in `js/net/src` lets a
producer serve that miss on demand. Add it so the publisher can answer FETCH
misses from memory or OPFS after relay eviction. Keep the bounded bytes in an
LRU and use the same timeline-before-delete ordering for DVR retention.

Ship the contract in the `@moq/*` packages. A dashboard browser-to-HLS proof
remains downstream (moq.pro) work.

## Required

- [Recording format](/quest/m1/archive/format.md)
- [Archive catalog](/quest/m1/archive/catalog.md)
- [Archive store](/quest/m1/archive/store.md)
- [Recording writer](/quest/m1/archive/writer.md)
