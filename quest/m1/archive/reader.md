# [L] Recording reader

## Goal

A reader takes an archive and a caller-provided `broadcast::Producer`, then
serves FETCH requests for every track and group advertised by the archive
timeline.

## Plan

Load the archive catalog and Window timeline through the ordinary track layout.
Use the timeline's track ranges to map `(track, group)` to its segment object;
there is no separate manifest or mutable head. Object listing bootstraps the
available timeline groups and follows a growing archive.

Use `track::Dynamic` to accept requested tracks and groups on the supplied
producer. GET one `(track, segment)` object, validate its format version and
every group/frame offset, and place it in a byte-bounded LRU. Adjacent group
FETCHes then reuse the same object. A request for one audio track or rendition
must never download another track's segment object.

Reproduce the original group sequence, frame timestamps, and payload bytes,
including a requested `frame_start`. A group absent from the timeline, a missing
object, or a malformed envelope behaves exactly like a group the source never
delivered; siblings and later segments remain usable.

The reader does not interpret media, own routing, or expose storage paths. It
also does not infer a terminal broadcast state. A caller such as a managed
recordings API may supply finality out of band; otherwise an archive may be
growing, crashed, or simply missing its tail and remains reloadable.

## Required

- [Archive catalog](/quest/m1/archive/catalog.md)
- [Archive store](/quest/m1/archive/store.md)
