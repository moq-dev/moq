# [M] Front eviction

## Goal

A group grows without a frame-count or byte cap. It sheds whole frames from
its front once its live tail passes 32 MB (`MAX_CACHE_BYTES`), or sooner when
the cache pool is over budget and the group can't go whole, because it is the
live edge or is being read. A reader that reaches a shed frame asks for a new
group as a joiner would, on tracks that opt into refresh, and otherwise gets
`Error::Evicted`; serving shed frames again is
[Flights](/quest/m1/unbounded-groups/flights.md).
`MAX_GROUP_FRAMES` is removed, and `MAX_CACHE_BYTES` stops capping a group
and bounds its live tail instead. The frame cap stays
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
  absolute indices. A group copy is one log run from a base that only
  advances: front eviction raises the base through `log::Writer::evict`
  (`rs/kio/src/log.rs`). Nothing is written below the base (decided
  2026-10-28, maintainer): refilled frames go into a separate copy, a
  [flight](/quest/m1/unbounded-groups/flights.md), never into runs beside
  the live log.
- Two triggers shed a group's front (decided 2026-10-28, maintainer). A
  write that takes the live tail past 32 MB sheds its oldest frames until it
  fits, with or without pool pressure. Pool debt evicts whole cold groups
  first (`pay_debt` in `model/track.rs`, as today) and then sheds front
  frames from a group that is protected or being read. The 32 MB trigger
  exists because the relay's default pool has no byte capacity and the
  latest group is exempt from idle expiry: shedding on pool debt alone would
  let a default relay hold one live group without bound, which the old caps
  prevented.
- A reader never starts mid-group at whatever the cache still holds: an
  opt-in tail join was rejected (decided 2026-10-28, maintainer). Joiners
  resync at a group boundary, as delta-compressed tracks expect.
- A reader that reaches a shed frame, whether it joined late or fell behind
  mid-read, asks for a new group as if the group were over the
  `Info::refresh` ratio, on tracks that opt into refresh, and otherwise gets
  `Error::Evicted` (decided 2026-10-24 for joiners, extended to readers
  that fall behind 2026-10-28, maintainer). Until Flights lands, no shed
  frame is fetched again.
- The live edge is paced by its fastest reader (decided 2026-10-25,
  maintainer, superseding 2026-10-24's "never paced by a reader"):
  [Fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md)
  builds it after this quest. Keep the design compatible: a slower reader
  still falls behind and loses its front. Pacing by the slowest reader stays
  rejected: one reader would stall the publisher.
- Groups are bounded by bytes, not frame count (decided 2026-10-24,
  maintainer). Each frame's storage slot is charged to the pool and to the
  32 MB, so empty frames still cost their slot once `MAX_GROUP_FRAMES` goes.
  Today slots are charged only through `VecDeque` capacity growth
  (`reserve_frame` in `model/group.rs`); on the log, charge each segment's
  slots when it is allocated. Storing a group as flat header and payload
  bytes was rejected: it copies every small frame, and lite and IETF encode
  headers differently.
- Downstream encoders that roll groups by the removed constants (moq-json,
  moq-mux TS, moq-stats) keep their own local roll triggers if they still
  need one. The [m1 compression migration](https://github.com/moq-dev/moq/pull/7046)
  deleted moq-flate; recheck the surviving JSON stream budget and track
  compression paths instead of retaining a deleted helper's cap.
- Compressed groups are not special (decided 2026-10-28, maintainer): they
  shed under the same two triggers, with no separate budget and no split
  into a later quest. They keep the prefix-before-inflate contract
  ([compressed tracks](/doc/concept/compression.md)): a relay inflates a
  group's plain copy as its frames arrive, carrying the decoder state past
  the shed front, so live plain readers keep reading, and the plain copy
  sheds under the same rules. A plain reader whose group's first frame was
  shed before its first inflate refreshes or gets `Evicted` like any other
  joiner. Today `GroupState::inflate` inflates on the first plain read and
  refuses a nonzero cached offset. Test a long compressed group through a
  small pool with a live plain reader and legacy-wire egress.
- Benchmarks: keep `group.rs` flat. Add a case swept over frames held and
  frames shed.

Public API: removes `MAX_GROUP_FRAMES`, and `MAX_CACHE_BYTES` stops capping
groups and bounds a group's live tail instead. A reader that used to see
`GroupTooLarge` now refreshes or sees `Evicted`. Wire: none.
