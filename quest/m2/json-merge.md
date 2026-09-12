# [S] Consume JSON snapshot patches instead of cloning them

## Goal

A snapshot consumer/encoder step on catalog-sized documents does not clone
patch nodes into the baseline. Reconstructed `Value` is identical.

## Plan

After each delta, `moq-json` `snapshot/encoder.rs` calls
`json_patch::merge(&Value)`, which clones patch nodes into `last`. The
existing `rs/moq-json/benches/codec.rs` already compares `merge_owned` vs
`json_patch::merge`. Production still uses the cloning merge.

Switch production merge to the consuming `merge_owned` already in the bench
(or equivalent) only if `decode_patch` shows a real gap on catalog-sized
fixtures. Do not change wire bytes.

Acceptance: existing `codec.rs` `decode_patch` / `consumer` / `baseline`.
Consumer step down on `big_static` / hang-catalog-sized docs. Identical
reconstructed `Value`. A measured no-win abandons the quest.

## Related

- [Benchmark comparisons](/quest/m2/performance-comparisons.md) - reporting conventions
