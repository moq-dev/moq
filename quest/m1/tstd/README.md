# T-STD compliant TS export

## Goal

`moq export ts` output passes a full ISO 13818-1 T-STD buffer-model check
(transport, multiplex, and elementary buffers, with every access unit decoded
at its DTS without overflow or underflow), on a clean path and under
sustained loss. That is the bar for the media-aware lane to carry primary
distribution. Without it, only the passthrough lane can.

## Plan

Decided (2026-09-30), from a discussion with t0ms and Gwendal's email: a
TS export has to be a proper remux, not an interleave of demuxed tracks.
Padding, pacing, and muxing all assume a fixed delay, so the export gets one
first. t0ms is testing whether T-STD compliance is feasible at all; record
the result here. If it isn't, re-plan this line.

The full-model check (`tstd` in `test/ts/compliance.py`) fails today's export
for two reasons, both delivery timing: 7-10 % of video access units finish
arriving after their DTS, and audio runs ~230 ms ahead in bursts that overflow
its TB and B. The media itself fits its buffers, so nothing yet says the bar
is out of reach.

This README owns the end-to-end proof: the #4613 netem rig (10% loss, a real
~10 Mb/s broadcast TS) passes the strict T-STD check, and the recipe runs
nightly.

## Required

- [Fixed-delay release](/quest/m1/tstd/delay.md) - frames go out at media time plus a fixed `--delay`, in one order under loss
- [TS byte schedule](/quest/m1/tstd/byte-schedule.md) - PCRs sit on the byte grid the mux rate implies, paced against the fixed delay
- [Send-ahead within the delay](/quest/m1/tstd/send-ahead.md) - total lag is `--delay`, send-ahead included, with a 1 s default
- [Mux-rate hold](/quest/m1/tstd/mux-rate-hold.md) - import publishes its catalog once the mux rate is measured, so export is constant-rate from the start

## Related

- [TS passthrough](/quest/m1/ts-passthrough.md) - the passthrough lane named in the Goal
