# [M] Readers of a streaming frame share its byte owners

## Goal

Reading a frame that is still arriving allocates per publication of new
bytes, not per reader per read, so a relay fanning a large frame out to many
subscribers pays no allocation that grows with its subscriber count.

## Plan

`FrameContent::slice` (`rs/moq-net/src/model/frame.rs`) wraps the `Fixed`,
`Growing`, and open `Chain` segment in a fresh `Bytes::from_owner(Filled { .. })`
on every call, and `from_owner` boxes its owner. Complete frames are already
free: a frozen frame hands out clones of one `Payload`.

Measured in the 2026-10-27 audit on main
[`651c30fee9`](https://github.com/moq-dev/moq/commit/651c30fee9d88fd56f4079433dd3e1f98d77f21c)
with a counting allocator: a 16 KiB frame arriving in sixteen 1 KiB chunks,
read by 128 readers after each chunk, cost exactly one 24-byte allocation per
reader per chunk (2,048 allocations, 48 KiB). Four publishers × 128 readers
cost 8,192. The same reads of complete frames allocated nothing.

Direction: readers that slice at the same written length share one owner,
for example the last `Bytes` built for the current watermark, cached beside
the storage and replaced when `written` advances. Constraints:

- An owner must never cover bytes past `written`: the writer is still
  filling them, so a shared slice over them is a data race, not just stale
  data.
- A `Growing` segment is replaced when it outgrows itself. A cached owner
  must not pin a superseded segment past the readers that hold slices of it.
- Sealed `Chain` segments already share one `Bytes` each; keep that.

`Segment::new` zero-fills its storage (`vec![0u8; capacity]`) before the
copy from the wire overwrites it. It's the same storage, so measure that fill
here too. Skipping it needs `MaybeUninit` storage and proof that every exported
slice is initialized, including overlapping writes and growth. Land it only if
the profile shows the fill.

Extend the frame and group benches with publishers × readers × arrival chunks,
including readers that hold old chunks across growth, and schedule them
nightly. Report allocations and instructions per read, with the counting
allocator from `rs/moq-net/benches/memory.rs`.

Public API: none expected. Wire: none.

## Related

- [Ingest batch](/quest/m2/perf/ingest-batch.md) - the write side of the same frames
- [Relay allocations](/quest/m2/perf/relay-allocs.md) - the relay-wide allocation profile this site belongs to
