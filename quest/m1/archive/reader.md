# [L] Recording reader

## Goal

A reader takes an archive and a caller-provided `broadcast::Producer`, then
serves FETCH requests for every track and group the archive timeline
advertises.

## Plan

Bootstrap by listing the timeline track's `segments/` keys, sorting them, and
replaying from a retained Window checkpoint. Records derive each track's
`groups/<largest>.<smallest>` key from its minimum and maximum advertised IDs.
Follow a growing archive with GET of timeline segment N+1; media listings do
not need refreshing. Not Found is not finality. A reader behind DVR retention
re-bootstraps from a retained checkpoint, and timeline pops evict cached ranges.
Alternatively enumerate timeline keys after the last replayed key with
`list_with_offset`, consuming and sorting the full result before replay.
Pagination tokens continue one enumeration, not future refreshes.

Filename listings provide a cached group-range index without media GETs. A
backend with verified ordered offset listing may find a cold FETCH candidate
using `PaginatedListStore` with offset `groups/<requested-group>` (19 padded
digits, no dot) and `max_keys = 1`. Check the lower bound and committed timeline
membership; the table resolves internal gaps. Generic listings are unordered,
so never take their first entry as the nearest match. A stale listing cannot
veto a successful GET referenced by a newer durable timeline record.

Use `track::Dynamic` (`rs/moq-net/src/model/track.rs:1652`, minted by
`Producer::dynamic` :1587) to accept requested tracks and groups on the
supplied producer; a cache-miss `Consumer::fetch_group` (:2352) parks on it.
Map `(track, group)` to its range-named object through the cached index, GET
that one object, validate the envelope version and every group/frame table
entry against the retrieved length, filename bounds, and exact committed
ranges, and place it in a byte-bounded LRU.
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
