# [M] JS unbounded groups

## Goal

Finish removing the JS frame-size cap while bounding retained memory.
The group frame-count and cumulative byte limits are removed by the draft
[moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411), which retains a
32 MiB tail, sheds its front, and gives lagging readers refresh-or-`Evicted`.

## Plan

Decided 2026-10-27 (maintainer): group cap removal waited only on Rust
front eviction, since it needed nothing from byte holes. Chunked frame
reads and byte-offset wire changes moved to
[JS byte holes](/quest/m1/unbounded-groups/js-byte-holes.md).

Decision needed: the current whole-frame API retains a 32 MiB allocation
guard, and the wire reader independently refuses reads past 64 MiB
(`Reader.#fillTo` and `Cursor.#ensure` in `js/net/src/stream.ts`). Removing
the frame guard alone neither supports arbitrary frames nor keeps a single
frame within the tail budget. The draft preserves this guard.

Recommendation: move frame-cap removal into JS byte holes, together with
chunked storage and reads, and complete this quest for groups only.
Alternatively, expand this quest to include bounded chunked frame storage
and reads before removing the frame guard.

If [Rust-backed @moq/net](/quest/m2/rs2ts/README.md) is adopted first, expose
its frame and cache contract instead.
