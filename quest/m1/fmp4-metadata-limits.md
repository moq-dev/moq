# [M] fMP4 sample plans, trun counts, and trak counts are bounded

## Goal

An fMP4 `moov` or `moof` that fits the importer's bounds
(`fmp4::MAX_HELD_BYTES`, `fmp4::MAX_FRAGMENT_SAMPLES`) parses and plans in
memory near `MAX_HELD_BYTES` and in time linear in its size, rather than
hundreds of megabytes or minutes of CPU: a `moof`'s sample plan is bounded
in bytes, a `trun`'s sample count is refused before mp4-atom expands it, and
a `moov`'s tracks are bounded and cost linear time. Each case below has a
test that asserts its bound.

## Plan

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7313](https://github.com/moq-dev/moq/pull/7313)): the fixes land
here, in m1, not in #7313, because 116 bytes cost 584 MB on any fMP4 or HLS
ingest.

- ✅ Land #7313 as-is, with the fixes in an m1 quest
- Land #7313 as-is, with the fixes staying in m2
- Fix all three in #7313
- Accept the numbers

Earlier, decided 2026-10-31 (maintainer, from
[moq-dev/moq#7232](https://github.com/moq-dev/moq/pull/7232)): measure the
parse amplification #7232's bounds allow, and fix each case or accept it with
the number. [moq-dev/moq#7313](https://github.com/moq-dev/moq/pull/7313)
added the media fuzz targets but cannot measure this: libFuzzer inputs stay a
few KiB, far below the 32 MiB bound. It measured the cases below by hand
instead, through `Import::decode` with moq-mux's counting allocator, as peak
live heap. Every one needs a fix:

| Input | Size | Peak | Note |
|---|---|---|---|
| `moof` with field-less `trun`s declaring 2^21 samples (`MAX_FRAGMENT_SAMPLES`), then an `mdat` header | 116 B | 584 MB | the plan alone, before any `mdat` byte |
| `moof` of 2^21 4-byte `trun` entries (`sample_size` only), then an `mdat` header | 8 MiB | 667 MB (80x) | 684 MB with passthrough |
| `moof` of 4-byte entries filling `MAX_HELD_BYTES` (8.4M entries) | 32 MiB | 369 MB (11x) | refused as `TooManySamples` only after mp4-atom parses it |
| `moov` of repeated audio `trak`s (bbb.mp4's, 477 B each) | 0.24 / 0.48 / 0.95 MB | 35x | CPU quadratic: 0.25 s, 0.9 s, 3.9 s; allocations 96 MB, 360 MB, 1.4 GB |

- Planning costs about 280 B per sample: a `Sample` in each `Run`, a
  `Chunk` in the `mdat`'s queue, and the growth of both unsized collections.
  Bound the plan's memory, not just the sample count: for example lower
  `MAX_FRAGMENT_SAMPLES` until the plan fits near `MAX_HELD_BYTES`,
  presize the collections, and shrink `Chunk` (its `Text` variant sets its
  size).
- mp4-atom expands each explicit `trun` entry to a 40-byte `TrunEntry`
  before the sample count is checked. Refuse the count before parsing it:
  read the `trun` sample counts from the held body, or bound a `moof` body
  separately from a `moov` one.
- Each `trak` added to a `moov` costs time and allocation in proportion to
  the tracks already added. Extrapolated to 32 MiB (about 70,000 `trak`s),
  that is over an hour of CPU and a peak above 1 GB. Find the step that
  revisits every earlier track (catalog renditions, track creation, or
  enrollment), make it linear, and bound the tracks a `moov` declares.

The bounds are moq-mux's public consts. This quest sets their values;
[one policy for oversized container metadata](/quest/m2/import-metadata-policy.md)
documents them later with MKV's.

Public API: possibly new or changed `pub const` bounds in moq-mux. Wire: none.

## Related

- [One policy for oversized container metadata](/quest/m2/import-metadata-policy.md) - documents the importer bounds this quest sets
- [Media fuzz findings](/quest/m1/fuzz-findings/README.md) - the bugs the media fuzz targets found
