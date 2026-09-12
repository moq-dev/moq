# [M] Measure and reduce MPEG-TS packet copies

## Goal

Reduce measured packet-buffer and reader-adapter work during TS import while
preserving tracks, timing, metadata, input recovery, and published media. Keep
public APIs and wire formats unchanged.

## Plan

`Import::decode` copies caller input into scratch, makes a 188-byte packet copy,
and copies that packet through the shared Feed adapter into mpeg2ts. Its
`scratch.drain(..off)` runs once per decode after whole packets have been
consumed, usually shifting less than 188 trailing bytes; this is not evidence
of quadratic behavior. Measure copy costs with varied caller chunk sizes.

Investigate reducing packet and Feed copies while preserving the persistent
reader's PAT/PMT state and routing. Keep resynchronization, continuity and
discontinuity handling, partial packets, PES assembly, EOF behavior, and
SCTE-35/private sections intact. Do not replace the parser or bypass its
validation merely to meet an assumed copy budget. Annex-B assembly is separate.

Add Criterion cases using
`rs/moq-mux/src/container/ts/test_data/bbb.ts` and
`rs/moq-mux/src/container/ts/test_data/kyrion_mpeg2av_ac3.ts`, including small,
packet-aligned, and large chunks. Wire published-media/timestamp/metadata
comparisons and damaged/fragmented-input regressions into existing mux CI tests.
Exercise SRT integration if its code changes. Compare identical before/after
workloads; ffmpeg numbers are contextual, not a correctness or speedup gate.
Follow the measurement and no-win completion rules in the
[questline](/quest/m2/mux-copies/README.md).

Include a long-running feed that bounds retained scratch storage after consumed
packets are reclaimed; cursor-based parsing must not retain the whole stream.

## Related

- [TS stream liveness](/quest/m2/3489-ts-import-stream-liveness.md) - independent observability work
- [Annex-B assembly](/quest/m2/mux-copies/annexb.md) - codec work after PES reassembly
