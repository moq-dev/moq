# [S] moq import ts counts the feed's PCR and PTS errors

## Goal

The ingest counters gain `PCR_repetition_error`,
`PCR_discontinuity_indicator_error` and `PTS_error`, graded on the feed's own
PCR and PTS values, so an operator can tell a contribution encoder that
under-inserts either from a healthy one. `PCR_accuracy_error` stays out.

## Plan

Decided while planning [#1838](https://github.com/moq-dev/moq/issues/1838):

- **Graded on values, and documented as such.** Repetition and discontinuity
  compare consecutive PCR values on the PCR PID, not arrival times. That
  grades the encoder's insertion, which is what ingest can speak for; the
  network in front of the importer (SRT, UDP, a pipe) re-times arrival, and
  an arrival-based check would grade that instead. The docs name the domain
  beside the counters, because a value-domain pass is not a wire pass: in the
  campaign, PCR that passed on file failed on the wire until the groomer was
  fixed
  ([evidence §3.2](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/docs/evidence.md)).
- **`PCR_accuracy_error` is out.** ±500 ns is graded either against a
  constant-rate model of byte position, which holds only on a CBR feed, or
  against arrival on the wire, which is an analyser measurement; the
  campaign's acceptance table marks it the one P2 check software cannot grade
  ([T33](https://github.com/tdrapier-wbd/moq-mpegts-paper/blob/d4c7573f9519a1c9c4882fab2021fbf82616fdcc/lab/test-33-gate2-preparation.md)).
- **A signalled jump is legal.** The interval across a packet carrying
  `discontinuity_indicator` on the PCR PID is dropped from both checks (ISO
  13818-1 2.4.3.4); the importer already reads it in `timebase_break`.
  Counting it failed a conforming splice in the campaign's harness (T33). The
  33-bit wrap is modular, not a jump.
- **`PCR_discontinuity_indicator_error`** is a consecutive difference outside
  0 to 100 ms without the flag, backwards included. On values alone a run of
  missing PCRs and an unsignalled jump are the same difference, so one
  forward difference over 100 ms counts under both checks; the docs say so
  rather than guess which it was. **`PTS_error`** is more
  than 700 ms on the transport clock between PES carrying a PTS, per
  elementary stream. Verbatim streams with no cadence (SCTE-35, DVB
  subtitles) are left ungraded rather than watched against an invented
  number.
- The counters join the stream-wide and per-PID fields of
  [TS import health](/quest/m2/ts-import-health.md), same names, same module.
- Tests: a 60 ms PCR gap counts one repetition error, a signalled 500 ms
  jump counts nothing, the same jump unsignalled counts one of each, a placed
  wrap counts nothing, and a 1 s PTS gap on one PID counts one there and
  nowhere else.

## Required

- [TS import health](/quest/m2/ts-import-health.md) - the module, clock, and stats fields this extends
