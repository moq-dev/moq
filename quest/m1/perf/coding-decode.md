# [S] Reduce owned path and byte-string decode copies

## Goal

Reduce measured allocation and payload-copy costs for owned byte strings,
strings, and paths without changing decoded values, errors, or wire bytes.

## Plan

`rs/moq-net/src/coding/decode.rs` decodes `Vec<u8>` through
`Buf::copy_to_bytes` followed by `to_vec`; `String` consumes that vector.
The first operation can return a shared view for a contiguous `Bytes` input,
but may allocate and copy for another `Buf`. Do not count every call as a
payload copy or assume a twofold improvement on production readers.

Benchmark the actual reader input types, contiguous and chained buffers,
lengths 8 B through 1 KiB, and representative announcement messages. Compare
safe copying into an initialized vector with the current implementation.
Preserve bounds checks before allocation and UTF-8 validation. Keep owned
return types; borrowed decoding and new public APIs are outside this quest.

Register a Criterion target in `moq-net` for discovery by `just bench`.
Report allocations, copied bytes, and throughput with paired base/current
runs. Retain the current implementation if no useful win is measured.
Add normal-CI regressions for fragmented input, truncated lengths/payloads,
invalid UTF-8, empty values, and path normalization; run the existing net
fuzz targets with `just rs fuzz path` and commit any regression inputs.

## Related

- [Reader buffering](/quest/m2/stream-buffering.md) - JS receive assembly
- [Benchmark comparisons](/quest/m2/performance-comparisons.md) - measurement conventions
