# [M] T-STD compliant TS export

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

Measured (2026-10-01, #4645): with the fixed-delay jitter buffer, per-PID
admission against each PID's T-STD buffers, and PCRs at their byte position, a
clean-path round trip passes the strict T-STD check and TSDuck's ±500 ns
pcrverify at the default 500 ms delay, for the generated clip at 10 Mb/s and
2 Mb/s and for a 1080p encode filling a 9 Mbit CPB (`just test ts --hrd`, after
t0ms's recipe). A unit that cannot arrive by its DTS fails the export rather
than arrive late. t0ms's broadcast capture (PAFF H.264, MP2, AC-3, teletext)
is still to be re-graded on this head.

This README owns the end-to-end proof: the #4613 netem rig (10% loss, a real
~10 Mb/s broadcast TS) passes the strict T-STD check, and the recipe runs
nightly.
