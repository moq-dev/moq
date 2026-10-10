# [M] TS passthrough export writes the multiplex back byte-identical on a fixed delay

## Goal

`moq export ts --passthrough --delay <dur>` reads the track a broadcast's
`m2ts` catalog section names and writes the same bytes back out, paced on the
source's own PCR and released at a fixed delay like an SRT receiver. From the
first group it releases, the output is byte-identical to the input, less any
object dropped late, so it is exactly as T-STD-conformant as the source.
There is no JS player for it.

## Plan

Decided (2026-10-01), from a discussion with t0ms, and split from the TS
passthrough quest on #5003, whose import half it completes:

- The export reuses the TS export's jitter buffer (`rs/moq-mux/src/jitter.rs`),
  keyed on each object's PCR time instead of a DTS, which also spreads each
  object's bytes at the PCR-implied rate. It must pace on the source's PCR,
  not on arrival: a pacer that re-clocks on arrival moves the PCR-to-PTS
  offset over a long capture and fails the decoder buffers, even though every
  byte is intact.
- An object that misses its deadline is dropped and counted, as the release
  stage does. The output shows a continuity error there; passthrough never
  rewrites continuity counters.
- Source clock drift is handled by the fixed-delay release's clock recovery,
  shared with the demultiplexed export, not here.
- A flagged forward jump arrives as a break marker before the group it
  starts; the release keeps the jump in the output, since the bytes carry it.
- Passthrough gives 1+1 identity for free: two exporters fed the same objects
  emit identical TS packets for every object both release, continuity
  counters included; an object one leg drops late is a gap in that leg only.
  Aligning the legs in time is left to the `--sync` anchor planned for the
  demultiplexed export's 2022-7 legs, which passthrough can adopt. That is TS
  identity, not ST 2022-7 recovery, which also needs matching RTP headers from
  a coordinated RTP egress.
- Rust only.

Test: an export of a broadcast capture is byte-identical to the input from the
first released group, and the strict T-STD check gives the same verdict on
output and input. Two exporters fed the same objects with different arrival
skew emit identical bytes; when only one misses a deadline, its output is the
other's less that object's packets. A dropped object is counted, and the rest
still go out on time. Rerun the #4613 netem rig (10% loss, 120 s) against it.

Update `doc/bin/cli.md` for the flag, which today says `export ts` does not
read the passthrough track.

Public API: a new export mode; nothing existing breaks.

## Related

- [TS hitless](/quest/m2/ts-hitless.md) - the demultiplexed lane's 2022-7 legs and the `--sync` anchor
- [T-STD TS export](/quest/m1/tstd/README.md) - the jitter buffer and clock recovery this release shares
