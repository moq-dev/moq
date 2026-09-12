# [S] Consume owned JSON snapshot patches

## Goal

Reduce measured patch-application allocations in snapshot encoding and
decoding while preserving reconstructed values, emitted bytes, and errors.

## Plan

Both `snapshot/encoder.rs` and `snapshot/decoder.rs` in `moq-json` own a
patch before passing it by reference to `json_patch::merge`. The encoder
has already serialized that patch; the decoder has just parsed it.
`rs/moq-json/benches/codec.rs` contains a consuming `merge_owned` comparison.

Measure complete encoder and decoder steps on catalog-sized documents,
including ownership acquisition, serialization, compression, and parsing.
Compare the existing `decode_patch`, consumer, and baseline cases. Keep
fixture setup outside the timed interval, but do not exclude an ownership
copy that production would need.

Move a consuming merge into private production code only if end-to-end
measurements justify it. Match merge-patch semantics for object deletion,
nested objects, arrays, scalar replacement, null, and empty patches.
Share the production helper with the benchmark rather than maintaining a
second algorithm there. Preserve encoder grouping and compression state.

Add equivalence regressions to normal CI, including compressed and plain
multi-delta roundtrips. Retain paired allocations and throughput results;
a measured no-win completes the quest without a production change.
No public API or wire change is needed.

## Related

- [Benchmark comparisons](/quest/m2/performance-comparisons.md) - reporting conventions
