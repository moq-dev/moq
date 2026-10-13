# [M] TS export fits a full source at its own rate

## Goal

`moq export ts` spends no more packets than the source on clocks and audio
framing, so a CBR source that fills its multiplex with no null packets
re-muxes at its own `--mux-rate` instead of falling behind.

## Plan

Reported in [#5362](https://github.com/moq-dev/moq/issues/5362) point 3 and
verified against `rs/moq-mux/src/container/ts/export.rs`: with points 1 and 2
fixed, the export of a 25 Mb/s broadcast feed averages 25.017 Mb/s of
non-null packets. Against the source it spends about 40 packets/s on
separate PCR packets (the source carries its PCR in video packets) and about
60 on one audio frame per PES (the source packs about twelve MP2 and nine
AC-3 frames per PES).

Decided 2026-10-12:

- **PCR in the PCR PID's adaptation field.** When the clock is due in a slot
  that sends a PCR-PID packet, carry the PCR in that packet's adaptation
  field, sampled at its byte position as today. Fall back to the standalone
  `pcr_packet` only when the slot has none. The 25 ms grid stays; the
  comment at `lay` explains why the PCR is not sampled from the DTS.
- **Several audio frames per PES.** Pack consecutive frames of an audio PID
  into one PES while the PES fits the PID's B, timed by the first frame's
  PTS. This applies to every audio stream the export frames (AAC, MP2, AC-3,
  E-AC-3), not only DVB AC-3.
- Land after [#5363](https://github.com/moq-dev/moq/pull/5363), which reworks
  the DVB AC-3 framing this packs.
- Test: a generated CBR source with no nulls, at the source's own
  `--mux-rate`, exports for the whole clip and passes the strict T-STD check.

## Closes

- [#5362](https://github.com/moq-dev/moq/issues/5362) - a full-rate CBR contribution feed exports at its own rate

## Related

- [TS export admits video at its full leak](/quest/m1/tstd/video-leak.md) - #5362 point 2
