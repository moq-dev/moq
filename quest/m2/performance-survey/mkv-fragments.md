# [M] Fragmented MKV import avoids replaying accumulated input

## Goal

Streaming MKV/WebM import costs remain reasonable as the same input is
split into smaller reads, without changing accepted media or parse errors.

## Plan

[Import::drain](/rs/moq-mux/src/container/mkv/import.rs) clones the retained
BytesMut, constructs a fresh WebmIterator and skips handled tags only after
parsing them (lines 227-252). A B-byte partial block arriving in c-byte
chunks can copy O(B²/c) bytes before completion; replay can add parse work.
This is before the reorder queues covered by the
[release audit](https://github.com/moq-dev/moq/pull/6311); recheck its final
scope before implementation.

Benchmark block size × input chunk size with identical complete media,
including large keyframes, lacing, bounded BlockGroups and unknown-size live
containers. Measure instructions/input byte, allocation and copy volume,
latency to first frame and peak memory. Separate cold catalog startup from
steady parsing. Include fragmented malformed/truncated tags in correctness
checks and ensure bytes, timestamps and errors match unsplit input.

Prefer retained parser progress or bounded incremental staging if it wins.
Do not hide the slope with a minimum read size or an arbitrary refusal cap.
This specializes the recurring audit's generic MKV importer coverage.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
