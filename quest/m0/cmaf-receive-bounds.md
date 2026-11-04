# [M] Subscriber-side CMAF parsing is bounded

## Goal

A hostile publisher can't make a subscriber allocate hundreds of megabytes
from one frame. Every place a subscriber parses received CMAF with mp4-atom
refuses any top-level box whose body exceeds `MAX_METADATA_BYTES`, and any
count over the importer's cap, before parsing it. A peak-memory test like
[moq-dev/moq#7687](https://github.com/moq-dev/moq/pull/7687)'s shows the
bounds holding on each path.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- ✅ m0 (security: one hostile frame costs about 320 MB per subscriber)
- m1
- m2

#7687 (merged) bounds the importer (publisher side). Received frames, up to 32 MiB
each, still reach mp4-atom unchecked:

- `fmp4::Wire::decode` and its `decode` helper
  (`rs/moq-mux/src/container/fmp4/mod.rs`, around line 556).
- `Wire::from_init` (same file, around line 440).
- MSF's init parse, `cmaf_moov`
  (`rs/moq-mux/src/catalog/msf/consumer.rs`, around line 416).
- `rs/moq-mux/src/container/mp4.rs` (the `Moof::decode` around line 162 and
  the init's `Moov::decode`).

- **Every box, not only `moof` and `moov`.** These paths decode each
  top-level box through `mp4_atom::Any::decode_maybe` before discarding the
  ones they don't use, and mp4-atom expands any box it knows (a stray
  top-level `trun`, say) wherever it appears. So read each top-level box's
  header and refuse or skip any body over #7687's `MAX_METADATA_BYTES` before
  `Any` sees it, keeping the limit one constant shared with the importer.
- **Counts, not only bytes.** A small box can still declare a large count.
  Bound received sample counts as the importer does (`MAX_FRAGMENT_SAMPLES`),
  including runs without per-sample fields. `sgpd`'s zero-byte entries are
  fixed upstream and picked up by
  [moq-mux on a fixed mp4-atom](/quest/m1/fuzz-findings/mp4-atom-bump.md),
  which waits on a release. If that hasn't landed, refuse an `sgpd` entry
  count the input can't hold here too.
- Measure each path's peak with #7687's counting allocator
  (`allocation::Usage::peak`), as its `fragment_plans_within_the_held_bound`
  does, with both a large-box and a small-box/large-count input per path, each
  failing without the fix.

Public API: none expected (a new error variant at most). Wire: none.

## Related

- [One policy for oversized container metadata](/quest/m2/import-metadata-policy.md) - the importer-side rule this mirrors
- [moq-mux on a fixed mp4-atom](/quest/m1/fuzz-findings/mp4-atom-bump.md) - the upstream `sgpd` count fix
