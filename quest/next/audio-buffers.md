# [M] Measure and reduce audio packetization copies

## Goal

Audio packetization and decoding avoid unnecessary per-packet allocation and
front-of-buffer movement without changing the settled frame or codec APIs.

## Plan

Producer::publish_full_frames drains the front of a Vec into another Vec for
each packet; Resampler also drains front chunks. Large PCM submissions repeat
that movement. Opus decode allocates space for a maximum-duration packet, then
truncates and converts it into another output buffer.

Measure allocations, copied samples, CPU, and latency for ordinary 20 ms input,
large caller batches, resampling, and multichannel PCM. Prefer cursors with one
compaction and reusable scratch storage where measurements justify them. Do not
turn borrowed scratch into output whose lifetime ends on the next codec call.

Use the existing audio quality/benchmark infrastructure. Preserve timestamps,
priming, DTX, partial packets, final padding, and discontinuities in CI regression
fixtures. Report measured changes without hard-coding machine-specific timing
thresholds into unit tests. Public API and wire: unchanged.

## Required

- [Audio configuration](/quest/main/audio-config.md) - optimize the settled PCM/codec boundary

## Related

- [Audio quality](/quest/next/audio-quality-harness/README.md) - end-to-end quality and latency evidence
