# [L] Front eviction

## Goal

A group grows without a frame-count or byte cap. When the cache pool is over
budget and the group can't go whole, because it is the live edge or is being
read, it sheds whole frames from its front. A reader that needs an evicted
frame gets it refilled in place. The refill comes from upstream over a
frame-precise range, or from the publisher's `track::Dynamic` handler, and
the reader gets `Error::Evicted` only when neither exists.
`MAX_GROUP_FRAMES` and the group byte cap are removed. The frame cap stays
until [Byte holes](/quest/m1/unbounded-groups/byte-holes.md). A moq-json
`Stream` accepts appends for its track's life: its encoder's pre-check against
the removed constants goes with them, and so do the docs promising "8192
records".

## Plan

See the [epic](/quest/m1/unbounded-groups/README.md) for the decisions.

- Storage takes the
  [append-only log](/quest/m2/perf/kio-channel-contention/log.md)'s layout,
  under today's mutex (recommended by the group storage benchmark,
  [moq-dev/moq#6627](https://github.com/moq-dev/moq/pull/6627), pending the
  maintainer; numbers in the
  [kio contention epic](/quest/m2/perf/kio-channel-contention/README.md)).
  Frames keep absolute indices. The tail run is today's `VecDeque` from a
  base that only advances: front eviction pops its front and raises the
  base. Refilled frames go into sorted runs below the base and never rejoin
  the tail run, so the log can later swap the tail for lock-free storage
  without touching refill. Readers keep one absolute cursor and search
  the refill runs only when it falls below the base. Prototyped this way, the common
  single-run group cost what it does today.
- Pool debt evicts whole cold groups first (`pay_debt` in
  `model/track.rs`, as today). Front frames are shed only from a group
  that is protected or being read.
- A refill writes a run strictly below the base, so it never races
  the tail writer. Coalesce concurrent readers on one hole into a
  single request, as `PendingFetch` does. Fold today's `Slot.head` tail copy
  into this.
- A refill must not evict what it was fetched for before the waiting reader
  takes it: hand its first demanded frame to that reader, or backpressure
  the refill on the reader's cursor. Test a `track::Dynamic` handler that
  writes more than the pool synchronously, so the reader still progresses.
- Refill goes through a `track::Dynamic` fill. Over
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

- [Segmented frames](/quest/m1/unbounded-groups/segmented-frames.md) - frame storage the cache can account for without whole-frame preallocation

## Related

- [Append-only log](/quest/m2/perf/kio-channel-contention/log.md) - later replaces the tail run with lock-free cursor reads
