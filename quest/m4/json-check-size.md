# [S] moq-json's size check costs the delta, not the document

## Goal

`moq_json::snapshot::Decoder` enforces `max_size` after a delta without
re-serializing the whole document each time, so the check's cost follows the
delta's size, and a benchmark shows the difference on a realistic stats
document. Not here: changing how `max_size` is configured or what it bounds.

## Plan

From the moq.pro stats and billing audit
([moq-dev/moq.pro#4513](https://github.com/moq-dev/moq.pro/pull/4513)
follow-ups). `Decoder::check_size` (`rs/moq-json/src/snapshot/decoder.rs`)
runs `serde_json::to_writer` over the whole current value after every
delta.

Decided 2026-11-26 (maintainer, /quest-plan of the moq.pro#4513
follow-ups): ✅ plan it, benchmark first (rejected: fix without a benchmark;
dropping it).

- Benchmark first, with a document shaped like an aggregated stats bucket
  and small deltas. If the re-serialization does not show up, delete this
  quest with the numbers in the PR instead of landing the change.
- Recommended fix: a merge patch grows the compact document by at most
  roughly the patch's length, so keep an upper bound (the last exact size
  plus each patch's length, with the separator slack the implementer
  derives) and serialize exactly only when the bound crosses the limit,
  resetting the bound to the exact size. Test a patch sequence that crosses
  the limit only through accumulation.

Public API: none. Wire: none.
