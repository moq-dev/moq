# [M] Measure and reduce complete access-unit assembly

## Goal

Reduce measured Annex-B ingestion and assembly work on verified complete access
units while preserving parsing, parameter sets, grouping, emitted media, and
error behavior. Keep public APIs and wire formats unchanged.

## Plan

`Split::decode` copies input into its tail and assembles output access units.
`NalIterator::copy_to_bytes` on `Bytes`/`BytesMut` splits shared storage, and
`Bytes::clone` in `Avc1::transform` does not copy payload. Measure the actual tail,
assembly, and length-prefix output copies rather than counting those operations
as extra NAL copies.

RTC's H.264 bridge calls Split on reassembled frames. Explore an internal path
for known complete access units that preserves validation, SPS/PPS handling,
keyframe detection, start-code normalization, and parameter-only behavior.
Shared or differently sized start-code prefixes may still require an output
copy. Do not introduce a new exported fast-path API.

TS currently assumes one access unit per video PES and calls Split then flush;
retain its behavior unless fixtures establish that a proposed optimization is
equivalent, including multiple units, partial input, and discontinuities. FLV
already carries length-prefixed samples and does not run this splitter.

Add Criterion Split, Avc1-transform, and RTC bridge cases for representative
IDR/P frames, mixed start-code lengths, parameter-only input, and caller chunk
sizes. Wire parsing/output equivalence and malformed/boundary regressions into
existing mux/RTC CI tests. Follow the measurement and no-win completion rules
in the [questline](/quest/m2/mux-copies/README.md).

## Related

- [MPEG-TS ingest](/quest/m2/mux-copies/ts.md) - packet transport work outside access-unit assembly
