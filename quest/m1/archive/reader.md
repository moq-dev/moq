# [L] Recording reader

## Goal

A reader takes an archive and a caller-provided `broadcast::Producer`, then
serves FETCH requests for every track and group the archive timeline
advertises.

## Plan

Bootstrap from an object listing under the prefix: it names the timeline
track's segments and every track's `.info` and segments. Read the timeline
objects in order through the Window decoder, and follow a growing archive by
re-listing; there is no manifest, head, or completion marker. A record whose
objects the listing cannot back is not served.

Use `track::Dynamic` (`rs/moq-net/src/model/track.rs:1652`, minted by
`Producer::dynamic` :1587) to accept requested tracks and groups on the
supplied producer; a cache-miss `Consumer::fetch_group` (:2352) parks on it.
Map `(track, group)` to its segment through the timeline's track ranges, GET
that one object, validate the envelope version and every group/frame table
entry against the retrieved length, and place it in a byte-bounded LRU.
Adjacent group FETCHes reuse the same object. A request for one audio track or
rendition never downloads another track's object.

Reproduce the original group sequence, frame timestamps, and payload bytes,
including a requested `frame_start`. A group absent from the timeline, a
missing object, an unknown envelope version, or a malformed table behaves
exactly like a group the source never delivered; siblings and later segments
remain usable.

The reader does not interpret media, own routing, or expose storage paths. It
does not infer a terminal broadcast state: a caller such as a managed
recordings API may supply finality out of band, and the reader then finishes
the replayed timeline track. Otherwise an archive may be growing, crashed, or
missing its tail, and remains readable.

## Required

- [Archive catalog](/quest/m1/archive/catalog.md)
- [Archive store](/quest/m1/archive/store.md)
