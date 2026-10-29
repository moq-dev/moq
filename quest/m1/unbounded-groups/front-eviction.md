# [L] Front eviction

## Goal

A group grows without a frame-count or byte cap. When the cache pool is over
budget and the group can't go whole, because it is the live edge or is being
read, it sheds whole frames from its front. A reader that needs an evicted
frame gets it refilled in place. The refill comes from upstream over a
frame-precise range, or from the publisher's `track::Requests` handler, and
the reader gets `Error::Evicted` only when neither exists.
`MAX_GROUP_FRAMES` and the group byte cap are removed. The frame cap stays
until [Byte holes](/quest/m1/unbounded-groups/byte-holes.md). A moq-json
`Stream` accepts appends for its track's life: its encoder's pre-check against
the removed constants goes with them, and so do the docs promising "8192
records".

## Plan

See the [epic](/quest/m1/unbounded-groups/README.md) for the decisions.

- Storage is the lock-free append-only log (`kio::log`), which landed in
  [moq-dev/moq#6782](https://github.com/moq-dev/moq/pull/6782) (decided
  2026-10-24 by the maintainer over the group storage benchmark,
  [moq-dev/moq#6627](https://github.com/moq-dev/moq/pull/6627)). Frames keep
  absolute indices. The log is the tail run, from a base that only advances:
  front eviction raises its base through `log::Writer::evict`, which already
  exists (`rs/kio/src/log.rs`). Refilled frames go into sorted runs below
  the base, behind their own mutex, and never rejoin the log. Readers keep
  one absolute cursor, read the log lock-free, and search the refill runs
  only when the cursor falls below the base.
- Pool debt evicts whole cold groups first (`pay_debt` in
  `model/track.rs`, as today). Front frames are shed only from a group
  that is protected or being read.
- A refill writes a run strictly below the base, so it never races
  the tail writer. Coalesce concurrent readers on one hole into a
  single request, as `FillState` does. Fold today's `Slot.head` tail copy
  into this.
- A refill asks for exactly the hole: from the reader's missing frame to the
  first frame still cached, using the transport fill's private frame-precise
  range end instead of duplicating the live edge. If more is evicted while the refill runs,
  the reader hits the next hole and refills again, one RTT per hole.
- Decided 2026-10-25 (maintainer): the application API stays whole-group,
  but the transport may start a group at a frame offset as an optimization,
  and a front-eviction refill is one. The lite draft allows that start
  without holding the prefix. A relay asked for frame k of a group it holds
  nothing of widens to frame 0 upstream.
- Refills are unpaced (decided 2026-10-24, maintainer). Pacing a refill on
  its fastest reader, and shared "trains" of readers that merge into the
  run ahead, were rejected as not worth their complexity. `FillState`
  coalescing is the sharing. A merged reader sits at the group's front,
  the first victim, so merging would also thrash under steady pressure.
- A refill must not evict what it was fetched for before the waiting reader
  takes it: hand its first demanded frame to that reader. Test a
  `track::Requests` handler that writes more than the pool synchronously, so
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
  refresh (decided 2026-10-24, maintainer). The origin's `track::Requests`
  handler decides whether to serve old content; refresh is the fallback.
- The live edge is paced by its fastest reader (decided 2026-10-25,
  maintainer, superseding 2026-10-24's "never paced by a reader"):
  [Fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md)
  builds it after this quest. Keep the design compatible: a slower reader
  still falls behind, loses its front, and refills. Pacing by the slowest
  reader stays rejected: one reader would stall the publisher.
- Groups are bounded by bytes, not frame count (decided 2026-10-24,
  maintainer). Each frame's storage slot is charged to the pool, so empty
  frames still cost their slot once `MAX_GROUP_FRAMES` goes. Today slots are
  charged only through `VecDeque` capacity growth (`reserve_frame` in
  `model/group.rs`); on the log, charge each segment's slots when it is
  allocated. Storing a group as flat header and payload bytes was rejected:
  it copies every small frame, and lite and IETF encode headers
  differently.
- Refill goes through a `track::Requests` fill. Over
  moq-transport, a hole that covers whole objects refills; one inside an
  object is `Evicted`.
- Downstream encoders that roll groups by the removed constants (moq-json,
  moq-mux TS, moq-stats) keep their own local roll triggers if they still
  need one. The [m1 compression migration](https://github.com/moq-dev/moq/pull/7046)
  deleted moq-flate; recheck the surviving JSON stream budget and track
  compression paths instead of retaining a deleted helper's cap.
- Compressed groups keep the existing prefix-before-inflate contract
  ([compressed tracks](/doc/concept/compression.md)). Today
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
`Evicted`. Wire: none beyond the amended frame offset rule.
