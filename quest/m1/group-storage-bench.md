# [M] Benchmark group storage before choosing it

## Goal

A benchmark picks the storage behind `GroupState`'s frames before either
proposal lands: the [append-only log](/quest/m2/perf/kio-channel-contention/log.md)
(lock-free cursor reads, front eviction by an atomic base), or a ring buffer
in its place, against the sorted runs of
[Front eviction](/quest/m1/unbounded-groups/front-eviction.md) (in-place
refill below `committed`). The outcome is numbers and a recommendation, and
both quests then build on the chosen shape.

## Plan

Decided 2026-10-20 (maintainer): benchmark first; the maintainer leans
toward the log, or a ring buffer, because hot-path performance matters more
than evicting the middle of a group, which moved to m3
([Middle eviction](/quest/m3/middle-eviction.md)).

- Prototype each shape behind `GroupState`'s current read and write
  operations only, enough to bench; neither prototype merges.
- Workloads: append with 1, 16, and 1,024 cursor readers on 1, 4, and 16
  threads; one-frame and multi-frame (video-heavy) groups; a long-lived group
  under front eviction; and a refill of an evicted front run while the tail
  writer appends. Reuse `rs/kio/benches/contended.rs` and moq-net's
  `group.rs`, and the multi-frame `just bench-profile lock` shape the log
  quest needs anyway.
- Bench how each shape refills a front hole: runs patch in place; a log or
  ring buffer needs a side run below its base. Rejecting the refill is not an
  option: the [unbounded groups](/quest/m1/unbounded-groups/README.md) reader
  contract refills an evicted run and returns `Error::Evicted` only when no
  source has it. If the log wins, say what Front eviction's refill becomes.
- Record the numbers and the choice in the
  [kio contention epic](/quest/m2/perf/kio-channel-contention/README.md) and
  the [unbounded groups epic](/quest/m1/unbounded-groups/README.md).

Public API: none. Wire: none.

## Related

- [Segmented frames](/quest/m1/unbounded-groups/segmented-frames.md) - changes what a frame entry holds, not how a group stores its frames
