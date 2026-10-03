# [M] The TS harness grades the full T-STD buffer model

## Goal

`test/ts/compliance.py` models every T-STD buffer an elementary stream passes
through, not only the transport buffer. It fails a stream where any buffer
overflows or where an access unit hasn't fully arrived by its DTS, and the TS
recipe runs it strictly against `moq export ts` output in CI.

## Plan

Today the `tstd` check models only the transport-buffer (TB) smoothing stage.
It's a shape check that warns unless `--strict` is set (`test/ts/README.md`).
Extend it to the multiplex buffer (MB, for video) and elementary buffer (EB)
with the ISO 13818-1 leak rates and sizes for each stream type and level, and
remove each access unit at its DTS. Timing stays on the stream's own PCR
clock, so the result is deterministic for a file.

Validate the model before gating: a known-compliant reference TS passes, and
a deliberately bursty one fails. Where TSDuck or another maintained tool
already implements T-STD, prefer it over hand-rolled math.

Gate: strict `tstd` in the TS recipe for export output. Until
[fixed-delay release](/quest/m1/tstd/delay.md) lands, the check will likely
fail, so land it report-only and flip the gate in that PR.
