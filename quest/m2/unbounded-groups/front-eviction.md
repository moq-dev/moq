# [L] Front eviction

## Goal

A group grows without a frame-count or byte cap. When the cache pool is over
budget and the group can't go whole, because it is the live edge or is being
read, it sheds whole frames from its front. A reader that needs an evicted
frame gets it refilled in place. The refill comes from upstream over a
frame-precise range, or from the publisher's `track::Dynamic` handler, and
the reader gets `Error::Evicted` only when neither exists.
`MAX_GROUP_FRAMES` and the group byte cap are removed. The frame cap stays
until [Byte holes](/quest/m2/unbounded-groups/byte-holes.md).

## Plan

See the [epic](/quest/m2/unbounded-groups/README.md) for the decisions.

- The group's state holds sorted runs of frames in place of one
  `VecDeque` + `offset`. Readers keep one absolute cursor. A sequential
  reader should hold its run so a lookup stays O(1). The common single-run
  group must cost what it does today.
- Pool debt evicts whole cold groups first (`pay_debt` in
  `model/track.rs`, as today). Front frames are shed only from a group
  that is protected or being read.
- A refill writes a run strictly below `committed`, so it never contends
  with the tail writer. Coalesce concurrent readers on one hole into a
  single request, as `PendingFetch` does. Fold today's `Slot.head` tail copy
  into this.
- A refill must not evict what it was fetched for before the waiting reader
  takes it: hand its first demanded frame to that reader, or backpressure
  the refill on the reader's cursor. Test a `track::Dynamic` handler that
  writes more than the pool synchronously, so the reader still progresses.
- Refill goes through [Model ranges](/quest/m1/subscribe-ranges/model.md)'
  range requests. Over moq-transport, a hole that covers whole objects
  refills; one inside an object is `Evicted`.
- Downstream encoders that roll groups by the removed constants (moq-json,
  moq-flate, moq-mux TS, moq-stats) keep their own local roll triggers if
  they still need one.
- Benchmarks: keep `group.rs` flat. Add a case swept over frames held and
  runs evicted, and one over concurrent readers refilling a hole.

Public API: removes `MAX_GROUP_FRAMES`, and `MAX_CACHE_BYTES` stops capping
groups. A reader that used to see `GroupTooLarge` now refills or sees
`Evicted`. Wire: none; it uses the frame-precise ranges.

## Required

- [Segmented frames](/quest/m2/unbounded-groups/segmented-frames.md) - frame storage the cache can account for without whole-frame preallocation
- [Model ranges](/quest/m1/subscribe-ranges/model.md) - `track::Dynamic` receives frame-precise range requests for misses
