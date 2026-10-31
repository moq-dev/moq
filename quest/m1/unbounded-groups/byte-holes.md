# [M] Byte holes

## Goal

A frame of any size, such as a file delivered as one frame, passes through a
relay holding only a group's 32 MB tail. Sealed segments inside the frame
are shed like front frames, and a [flight](/quest/m1/unbounded-groups/flights.md)
can start at a `(frame, byte)` position on the current lite wip version.
`MAX_CACHE_BYTES` stops capping a frame; it still bounds a group's tail.

## Plan

See the [epic](/quest/m1/unbounded-groups/README.md) for the decisions.
Decided 2026-10-27 (maintainer): the byte offset targets the current lite wip
version (lite-07-wip today); the lite-07 cut does not wait on it. This follows
[lite-07's freeze policy](/quest/m1/lite07-finalize.md): wire quests target
whichever wip is current when they land, and its Required list is not a
closed freeze set. If lite-07 is cut first, the offset goes on the next wip
and lite-07 peers start flights only at frame boundaries, like older lite
versions. Decided 2026-10-28 (maintainer): shed frames are served again only
by flights, so this waits on Flights and stays in m1.

- Segments are `frame::SEGMENT` (1 MiB) long, so segment `i` always holds
  bytes from `i * SEGMENT`, and a completed chain is `frame::Payload::Chain`.
  Only a frame past one segment written in pieces chains. One written whole
  (`write_frame`, or a single write or transport chunk carrying all of it)
  stays one `Bytes` at any size, so evicting part of it needs it split into
  segment slices, whose memory frees only once every slice drops, or copied
  into segments.

- `Position` gains a byte offset within the frame. A lite range end can
  carry it, and so can a group stream header, beside `frame_start`. A
  stream that starts mid-frame still states the frame's full size. Older
  lite versions and moq-transport start flights only at frame or object
  boundaries. A reader inside a shed object refreshes or is `Evicted` there.
- Every sealed segment is evictable under the same policy as front frames,
  including one a reader is on. A reader keeps its current segment alive
  through its own reference, as prefetched frames are today. That memory
  is bounded by readers times one segment, not by the cache. Only the
  writer's open segment stays resident. A reader at a shed segment boards
  a flight that starts at its `(frame, byte)`.
- Route failover mid-frame (`frame::Consumer`'s `Recover`) requests only
  the missing suffix by byte, instead of refetching the frame from its
  start. Cover it with a regression: a route fails partway through a frame
  larger than the 32 MB tail.
- Update `drafts/draft-lcurley-moq-lite.md` and run `just drafts check`.
  Also drop the 32 MiB interop cap in `drafts/draft-lcurley-moq-e2ee.md` if
  it no longer follows from a model limit.
- Rust readers get a chunked payload read, so a frame larger than memory
  never has to be collected whole. [moq-dev/moq#6628](https://github.com/moq-dev/moq/pull/6628)
  kept its segments internal; the JS half is in
  [JS byte holes](/quest/m1/unbounded-groups/js-byte-holes.md).
- Test a frame several times the 32 MB tail through a relay chain: a late
  joiner reads it whole through a flight, and memory stays near 32 MB per
  copy.

Public API: `Position` gains a byte field, frames gain a chunked read, and
`MAX_CACHE_BYTES` stops capping a frame.
Wire: a byte offset on the current lite wip's range ends and group header.

## Required

- [Flights](/quest/m1/unbounded-groups/flights.md) - the paced copies a mid-frame start serves
