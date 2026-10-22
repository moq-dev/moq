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

- Storage comes from the
  [group storage benchmark](/quest/m2/group-storage-bench.md) (decided
  2026-10-20, maintainer): sorted runs of frames in place of one
  `VecDeque` + `offset`, or the
  [append-only log](/quest/m2/perf/kio-channel-contention/log.md) or a ring
  buffer, which the maintainer leans toward because performance matters more
  than middle eviction. Readers keep one absolute cursor. A sequential
  reader should hold its position so a lookup stays O(1). The common
  single-run group must cost what it does today.
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
- Refill goes through
  [Dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md). Over
  moq-transport, a hole that covers whole objects refills; one inside an
  object is `Evicted`.
- Downstream encoders that roll groups by the removed constants (moq-json,
  moq-mux TS, moq-stats) keep their own local roll triggers if they still
  need one. The [m1 compression migration](/quest/m1/compress/migrate.md)
  deletes moq-flate; recheck the surviving JSON stream budget and track
  compression paths instead of retaining a deleted helper's cap.
- Compressed groups keep the existing prefix-before-inflate contract
  ([compressed tracks](/quest/m1/compress/README.md)). Today
  `GroupState::inflate` refuses a nonzero cached offset. Front eviction must
  refill the needed compressed prefix before inflation or retain equivalent
  decoder state, with compressed storage, the shared inflated copy, and
  codec state all bounded by the pool. Never pin a whole growing prefix to
  make a late plain reader work. Test a long compressed group through a
  small pool with a late plain reader and legacy-wire egress.
- Benchmarks: keep `group.rs` flat. Add a case swept over frames held and
  runs evicted, and one over concurrent readers refilling a hole.

Public API: removes `MAX_GROUP_FRAMES`, and `MAX_CACHE_BYTES` stops capping
groups. A reader that used to see `GroupTooLarge` now refills or sees
`Evicted`. Wire: none; it uses the frame-precise ranges.

## Required

- [Group storage benchmark](/quest/m2/group-storage-bench.md) - picks the group storage this builds on
- [Segmented frames](/quest/m2/unbounded-groups/segmented-frames.md) - frame storage the cache can account for without whole-frame preallocation
- [Dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md) - `track::Dynamic` receives frame-precise range requests for misses

## Related

- [Append-only log](/quest/m2/perf/kio-channel-contention/log.md) - the other proposal for `GroupState` storage, decided by the same benchmark
