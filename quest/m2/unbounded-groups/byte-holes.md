# [M] Byte holes

## Goal

A frame of any size, such as a file delivered as one frame, passes through a
relay whose pool is smaller than the frame. Sealed segments inside the frame
are evicted like front frames and refilled by `(frame, byte)` position on the
next unpublished lite version after lite-07. The frame cap
(`MAX_CACHE_BYTES`) is removed.

## Plan

See the [epic](/quest/m2/unbounded-groups/README.md) for the decisions.

- `Position` gains a byte offset within the frame. A lite range end can
  carry it, and so can a group stream header, beside `frame_start`. A
  stream that starts mid-frame still states the frame's full size. Older
  lite versions and moq-transport refill only at frame or object
  boundaries. An object with a hole is `Evicted` there.
- Every sealed segment is evictable under the same policy as front frames,
  including one a reader is on. A reader keeps its current segment alive
  through its own reference, as prefetched frames are today. That memory
  is bounded by readers times one segment, not by the cache. Only the
  writer's open segment stays resident. A refilled segment reaches its
  waiting reader before it can be evicted, as Front eviction does for
  frames.
- Route failover mid-frame (`frame::Consumer`'s `Recover`) requests only
  the missing suffix by byte, instead of refetching the frame from its
  start. Cover it with a regression: a route fails partway through a frame
  larger than the pool.
- Update `drafts/draft-lcurley-moq-lite.md` and run `just drafts check`.
  Also drop the 32 MiB interop cap in `drafts/draft-lcurley-moq-e2ee.md` if
  it no longer follows from a model limit.
- Test a frame several times the pool size through a relay chain: a late
  joiner reads it whole, and memory stays near the pool size.

Public API: `Position` gains a byte field, and `MAX_CACHE_BYTES` is removed.
Wire: a byte offset on the next lite version's range ends and group header.

## Required

- [Front eviction](/quest/m2/unbounded-groups/front-eviction.md) - run-based group state and in-place refill
