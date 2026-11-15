# [M] Byte holes

## Goal

A frame of any size, such as a file delivered as one frame, passes through a
relay holding only a group's 32 MB tail. Sealed segments inside the frame
are shed like front frames, and a flight
([moq-dev/moq#7944](https://github.com/moq-dev/moq/pull/7944)) can start at a `(frame, byte)` position on the current lite wip version.
`MAX_CACHE_BYTES` stops capping a frame; it still bounds a group's tail.

## Plan

See the [epic](/quest/m3/unbounded-groups/README.md) for the decisions.
Decided 2026-10-27 (maintainer): the byte offset targets the current lite wip
version (lite-07-wip today); the lite-07 cut does not wait on it. This follows
[lite-07's freeze policy](/quest/m2/lite07-finalize.md): wire quests target
whichever wip is current when they land, and its Required list is not a
closed freeze set. If lite-07 is cut first, the offset goes on the next wip
and lite-07 peers start flights only at frame boundaries, like older lite
versions. Decided 2026-10-28 (maintainer): shed frames are served again only
by flights, so this waited on Flights, which landed in
[moq-dev/moq#7944](https://github.com/moq-dev/moq/pull/7944).

Moved to m3 2026-11-12 (maintainer, /quest-audit), with its epic: the cut does not wait on it, so if lite-07 is cut first the offset goes on the next wip, as above.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ the byte offset rides
FETCH's frame start plus the group header, not a SUBSCRIBE range end.

Decided 2026-11-13 (maintainer, /quest-complete):

- ✅ The byte stays crate-private until a public API needs it (rejected: add
  `Position::byte` now, about 90 literals across moq-net, moq-archive, and
  moq-ffi). `Position` also names subscribe ranges and largest locations,
  where a byte means nothing.
- ✅ [moq-dev/moq#8699](https://github.com/moq-dev/moq/pull/8699) merges its
  slice alone, and this quest keeps the rest (rejected: grow that PR to the
  whole quest).

[moq-dev/moq#8699](https://github.com/moq-dev/moq/pull/8699) landed a first
slice: a lite-07 FETCH `Start Byte`, a group copy starting partway into a frame
(`group::Bounds::byte` and a `Lead` outside the frame log), and `Recover`
asking only for a half-read frame's rest. Decided 2026-11-14 (maintainer, on
[moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ back it out
of lite-07, since frames are capped at 32 MiB and only a chunk reader saved at
most one frame's prefix per failover. The
[backout](https://github.com/moq-dev/moq/pull/8837) restored refetch-and-skip;
reverting it brings the slice back. This quest does not start until a consumer of frames
over 32 MiB is named.

What remains:

- Segments are `frame::SEGMENT` (1 MiB) long, so segment `i` always holds
  bytes from `i * SEGMENT`, and a completed chain is `frame::Payload::Chain`.
  Only a frame past one segment written in pieces chains. One written whole
  (`write_frame`, or a single write or transport chunk carrying all of it)
  stays one `Bytes` at any size, so evicting part of it needs it split into
  segment slices, whose memory frees only once every slice drops, or copied
  into segments.
- Restore the start-byte slice above on the current lite wip version.
- Every sealed segment is evictable under the same policy as front frames,
  including one a reader is on. A reader keeps its current segment alive
  through its own reference, as prefetched frames are today. That memory
  is bounded by readers times one segment, not by the cache. A reader holding
  a `Payload::Chain` holds every segment through its `Arc<[Bytes]>` today, so
  the chain has to hand a reader one segment at a time. Only the writer's open
  segment stays resident. A reader at a shed segment boards a flight that
  starts at its `(frame, byte)`: `FillState::fly` and `group::Bounds` carry
  the byte, and the flight's copy starts with a `Lead`.
- A group stream that starts mid-frame (a relay holding only a group's tail,
  once segments shed) carries the byte offset in the lite wip GROUP header
  beside `Frame Start`, and still states the frame's full size. Older lite
  versions and moq-transport start only at frame or object boundaries. A
  reader inside a shed object refreshes or is `Evicted` there.
- `MAX_CACHE_BYTES` stops capping a frame (`group::Producer::open_frame` refuses a
  larger one today). Then test failover mid-frame on a frame
  larger than the 32 MB tail.
- Drop the 32 MiB interop cap in `drafts/draft-lcurley-moq-e2ee.md` if it no
  longer follows from a model limit, and run `just drafts check`.
- Test a frame several times the 32 MB tail through a relay chain: a late
  joiner reads it whole through a flight, and memory stays near 32 MB per
  copy.

Public API: `Position` gains a byte field only once a public reader needs a
byte position, and `MAX_CACHE_BYTES` stops capping a frame. Wire: a byte
offset on the current lite wip's group header.

## Waiting

- Maintainer: name a consumer of frames over 32 MiB, since the frame cap makes this work save little until one exists (an agent cannot pick a product need). Check: this quest's Plan names it.
