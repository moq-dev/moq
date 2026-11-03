# [M] Subscriber-side CMAF parsing is bounded

## Goal

A hostile publisher can't make a subscriber allocate hundreds of megabytes
from one frame. Every place a subscriber parses received CMAF with mp4-atom
refuses a `moof` (or init `moov`) larger than `MAX_METADATA_BYTES` before
parsing it, and a peak-memory test like
[moq-dev/moq#7687](https://github.com/moq-dev/moq/pull/7687)'s shows the
bound holding on each path.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- ✅ m0 (security: one hostile frame costs about 320 MB per subscriber)
- m1
- m2

#7687 bounds the importer (publisher side). Received frames, up to 32 MiB
each, still reach mp4-atom unchecked:

- `fmp4::Wire::decode` and its `decode` helper
  (`rs/moq-mux/src/container/fmp4/mod.rs`, around line 556).
- `Wire::from_init` (same file, around line 440).
- MSF's init parse, `cmaf_moov`
  (`rs/moq-mux/src/catalog/msf/consumer.rs`, around line 416).
- `rs/moq-mux/src/container/mp4.rs` (the `Moof::decode` around line 162 and
  the init's `Moov::decode`).

Reuse #7687's `MAX_METADATA_BYTES`: read each box header's size and refuse
an oversized `moof`/`moov` before decoding it, so the limit stays one
constant shared with the importer. Measure each path's peak with #7687's
counting allocator (`allocation::Usage::peak`), as its
`fragment_plans_within_the_held_bound` does, failing without the fix.

Public API: none expected (a new error variant at most). Wire: none.

## Required

- [fMP4 sample plans, trun counts, and trak counts are bounded](/quest/m1/fmp4-metadata-limits.md) - lands `MAX_METADATA_BYTES` (moq-dev/moq#7687)

## Related

- [One policy for oversized container metadata](/quest/m2/import-metadata-policy.md) - the importer-side rule this mirrors
