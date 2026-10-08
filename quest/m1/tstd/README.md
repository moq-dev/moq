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
first.

Measured (2026-10-01, #4645): with the fixed-delay jitter buffer, per-PID
admission against each PID's T-STD buffers, and PCRs at their byte position, a
clean-path round trip passes the strict T-STD check and TSDuck's ±500 ns
pcrverify at the default 500 ms delay, for the generated clip at 10 Mb/s and
2 Mb/s and for a 1080p encode filling a 9 Mbit CPB (`just test ts --hrd`, after
t0ms's recipe). A unit that cannot arrive by its DTS fails the export rather
than arrive late. t0ms graded it on a CNN broadcast capture (PAFF H.264, MP2,
DVB AC-3, teletext, SCTE-35; 2026-10-02): at 1 s on loopback, across hosts and
under the #4613 rig at 0 % and 1 % loss, every track passed T-STD and
`compliance.py` with no late drops, once the clock anchored on the track sent
latest. At 500 ms a 540 s run still misses a video deadline about 157 s in,
which [send-ahead](/quest/m1/tstd/send-ahead.md)'s 1 s default addresses, and
10 % loss is still to pass.

This README owns the end-to-end proof: the #4613 netem rig (10% loss, a real
~10 Mb/s broadcast TS) passes the strict T-STD check, and the recipe runs
nightly.

## Required

- [Send-ahead within the delay](/quest/m1/tstd/send-ahead.md) - total lag is `--delay`, send-ahead included, with a 1 s default
- [Mux-rate hold](/quest/m1/tstd/mux-rate-hold.md) - import publishes its catalog once the mux rate is measured, so export is constant-rate from the start

## Related

- [TS passthrough](/quest/m1/ts-passthrough.md) - the passthrough lane named in the Goal
