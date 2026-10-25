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

- Storage comes from the
  [group storage benchmark](/quest/m1/group-storage-bench.md) (decided
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
- A refill asks for exactly the hole: from the reader's missing frame to the
  first frame still cached, over a frame-precise range end. Today's fills
  always run to the group's end (`end_frame: None`), which on a live group
  would duplicate the live edge. If more is evicted while the refill runs,
  the reader hits the next hole and refills again, one RTT per hole.
- Refills are unpaced (decided 2026-10-24, maintainer). Pacing a refill on
  its fastest reader, and shared "trains" of readers that merge into the
  run ahead, were rejected as not worth their complexity. `FillState`
  coalescing is the sharing. A merged reader sits at the group's front,
  the first victim, so merging would also thrash under steady pressure.
- A refill must not evict what it was fetched for before the waiting reader
  takes it: hand its first demanded frame to that reader. Test a
  `track::Dynamic` handler that writes more than the pool synchronously, so
  the reader still progresses.
- A refill of frames `0..N` refreshes the recency of the group's frames from
  `N+1` on (decided 2026-10-24, maintainer). Pressure then sheds the
  refilled run once read, before `N+1`, so a refill never evicts the frames
  its reader needs next. This is recency, not pinning: under enough
  pressure `N+1` still goes. Test a slow reader on a long live group in a
  small pool: it makes progress by refilling holes, never by refetching a
  frame it already had.
- When nothing serves the hole, the reader gets `Error::Evicted`, and a
  join onto a group whose first frame is evicted asks for a new group as if
  it were over the `Info::refresh` ratio, on tracks that opt into
  refresh (decided 2026-10-24, maintainer). The origin's `track::Dynamic`
  handler decides whether to serve old content; refresh is the fallback.
- The live edge is never paced by a reader (decided 2026-10-24,
  maintainer). A slow reader on a 1:1 track falls behind, loses its front,
  and refills, at up to twice the upstream bandwidth for that reader.
  Backpressuring the publisher like TCP was rejected: one reader would stall
  it.
- Groups are bounded by bytes, not frame count (decided 2026-10-24,
  maintainer). Each frame's storage slot is charged to the pool, so empty
  frames still cost their slot once `MAX_GROUP_FRAMES` goes. Today slots are
  charged only through `VecDeque` capacity growth (`reserve_frame` in
  `model/group.rs`); on the log, charge each segment's slots when it is
  allocated. Storing a group as flat header and payload bytes was rejected:
  it copies every small frame, and lite and IETF encode headers
  differently.
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

- [Group storage benchmark](/quest/m1/group-storage-bench.md) - picks the group storage this builds on
- [Segmented frames](/quest/m1/unbounded-groups/segmented-frames.md) - frame storage the cache can account for without whole-frame preallocation

## Related

- [Append-only log](/quest/m2/perf/kio-channel-contention/log.md) - the other proposal for `GroupState` storage, decided by the same benchmark
