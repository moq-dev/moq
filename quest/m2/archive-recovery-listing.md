# [M] Archive recovery listing

## Goal

A DVR writer resuming a recording lists storage in proportion to what changed
since its last checkpoint, not every stored `segments/` object, while still never
deleting a referenced object.

## Plan

- Recovery today (`recover` in `rs/moq-archive/src/recover.rs`) lists the
  whole recording, every track's `segments/`, before accepting input. Compare bounding it with an ordered
  `list_with_offset` from the oldest retained range, with orphans below it left
  to a background sweep, against a checkpointed listing marker.
- Benchmark recovery time and requests over archive size before and after.

## Related

- [Archive](/quest/m1/archive/README.md) - the line whose recovery this bounds
