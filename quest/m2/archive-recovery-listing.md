# [M] Archive recovery listing

## Goal

A DVR writer resuming a recording lists storage in proportion to what changed
since its last checkpoint, not every stored `groups/` object, while still never
deleting a referenced object.

## Plan

- Recovery today reconciles a complete `groups/` listing of every recorded
  track before accepting input. Compare bounding it with an ordered
  `list_with_offset` from the oldest retained range, with orphans below it left
  to a background sweep, against a checkpointed listing marker.
- Benchmark recovery time and requests over archive size before and after.

## Required

- [Archive](/quest/m1/archive/README.md) - the recovery this bounds ships with the line
