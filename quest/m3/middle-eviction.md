# [M] Middle eviction

## Goal

A group too large to keep whole holds its head, for fast joins (the keyframe
at frame 0), and its tail, for live readers, and evicts the runs between.
A joiner decodes from the cached head at once, and refills the middle only if
it reads that far.

## Plan

See the [unbounded groups epic](/quest/m1/unbounded-groups/README.md) for
the decisions. Decided 2026-10-20 (maintainer): moved to m3, because group
storage is chosen for hot-path performance, not for evicting the middle.
Group storage was decided 2026-10-24 (maintainer, over
[moq-dev/moq#6627](https://github.com/moq-dev/moq/pull/6627)): the
[append-only log](/quest/m1/unbounded-groups/log.md) is a tail run that sheds
only its front, from a base that only advances, plus refilled runs below it.
So a kept head moves into a run below the base before the base advances past
the middle, rather than splitting the tail.

- This reuses the storage and refill from
  [Front eviction](/quest/m1/unbounded-groups/front-eviction.md). The new
  part is choosing what to keep.
- Decide how much head to keep from what a join needs (e.g. the first frame,
  or what a fast join reads), not from a fixed constant. Within a group,
  the head is the last victim regardless of recency: middle runs go first,
  then the head, and only then the group. This is not pinning.
- Test a long-GOP live group under pressure: a joiner starts from the
  cached head with no upstream request, and a live reader is unaffected.

Public API: none beyond Front eviction. Wire: none.

## Required

- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - run-based group state and in-place refill
