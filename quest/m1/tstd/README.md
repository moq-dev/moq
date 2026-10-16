# [L] T-STD compliant TS export

## Goal

`moq export ts` output passes the applicable ISO 13818-1 T-STD buffer-model
checks for explicitly supported profiles, with admitted decodable access
units complete by their decode time and no modelled buffer overflow. Under
loss, validate the surviving content, declared discontinuities and recovery;
missing source units cannot all be decoded. The deferred packet-passthrough
export owns its timing and recovery proof before claiming readiness; it does
not gate the semantic export proof here.

## Plan

Decided (2026-09-30), from a discussion with t0ms and Gwendal's email: a
TS export has to be a proper remux, not an interleave of demuxed tracks.
Padding, pacing, and muxing all assume a fixed delay, so the export gets one
first.

Measured (2026-10-01, #4645): with the fixed-delay jitter buffer, per-PID
admission against each PID's T-STD buffers, and PCRs at their byte position, a
clean-path round trip passes the strict T-STD check and TSDuck's ±500 ns
pcrverify at the then-default 500 ms delay, for the generated clip at 10 Mb/s and
2 Mb/s and for a 1080p encode filling a 9 Mbit CPB (`just test ts --hrd`, after
t0ms's recipe). A unit that cannot arrive by its DTS fails the export rather
than arrive late. t0ms graded it on a CNN broadcast capture (PAFF H.264, MP2,
DVB AC-3, teletext, SCTE-35; 2026-10-02): at 1 s on loopback, across hosts and
under the #4613 rig at 0 % and 1 % loss, every track passed T-STD and
`compliance.py` with no late drops, once the clock anchored on the track sent
latest. At 500 ms a 540 s run still misses a video deadline about 157 s in,
which the 1 s default `--delay` addresses, and 10 % loss is still to pass.

This README owns the remaining end-to-end proof: make the #4613 netem rig
(10% loss, a redistributable or reproducible ~10 Mb/s broadcast-style TS)
pass the applicable strict T-STD checks, and run the recipe nightly. This
is a completion requirement, not a result established by the measurements above.

Audit clarification (2026-10-11): record what actually arrived, was
retransmitted, or missed its deadline. The IP loss percentage alone is not
a MoQ omission test. Keep this network/pacing proof and add deterministic
content-loss coverage through the TS reliability line. Run TSDuck, FFmpeg
software decode, and the custom model as automated assertions with known-bad
controls. A model pass is not independent proof of decoded content.

The current model does not cover every codec/layout. Name those exclusions and
assumptions in the profile report. Do not relabel the current subset a full
standards check.

## Related

- [TS passthrough export](/quest/m2/ts-passthrough-export.md) - the later passthrough lane owns its own output proof
- [Automated validation](/test/ts/README.md) - independent decode, profile coverage and CI ownership
- [Loss and recovery](/quest/m1/ts-reliability/recovery.md) - actual object/group omissions and recovery
