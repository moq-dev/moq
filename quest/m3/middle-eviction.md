# [M] Middle eviction

## Goal

A group too large to keep whole holds its head, for fast joins (the keyframe
at frame 0), and its tail, for live readers, and evicts the runs between.
A joiner decodes from the cached head at once, and refills the middle only if
it reads that far.

## Plan

See the [unbounded groups epic](/quest/m2/unbounded-groups/README.md) for
the decisions. Decided 2026-10-20 (maintainer): moved to m3, because group
storage is chosen for hot-path performance
([group storage benchmark](/quest/m2/group-storage-bench.md)), not for
evicting the middle; if that picks a log or ring buffer, keeping a head
apart from the tail is new storage work here.

- This reuses the storage and refill from
  [Front eviction](/quest/m2/unbounded-groups/front-eviction.md). The new
  part is choosing what to keep.
- Decide how much head to keep from what a join needs (e.g. the first frame,
  or what a fast join reads), not from a fixed constant. Within a group,
  the head is the last victim regardless of recency: middle runs go first,
  then the head, and only then the group. This is not pinning.
- Test a long-GOP live group under pressure: a joiner starts from the
  cached head with no upstream request, and a live reader is unaffected.

Public API: none beyond Front eviction. Wire: none.

## Required

- [Front eviction](/quest/m2/unbounded-groups/front-eviction.md) - run-based group state and in-place refill
