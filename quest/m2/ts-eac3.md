# [S] DVB E-AC-3 gets a buffer model in TS export

## Goal

`moq export ts` splits DVB E-AC-3 private-data PES (`stream_type` 0x06 with
an `enhanced_AC3_descriptor`) one access unit per PES, schedules each against
the E-AC-3 buffer ETSI TS 101 154 gives DVB, and `test/ts/compliance.py`
grades it. That's the same treatment DVB AC-3 got in
the fixed-delay export (#4645).

## Plan

Found in the fixed-delay export review: E-AC-3 carried as DVB private data still goes out unsplit,
with no buffer model, so a multi-frame PES can overflow B.

- Split it the way `rs/moq-mux/src/container/ts/export.rs` splits DVB AC-3, but per access unit: an
  independent sync frame plus the dependent substreams that follow it share
  one presentation interval (ETSI TS 102 366 Annex E), so they go out in one
  PES on one decode time. Read sizes from each sync frame's `frmsiz`, and
  drop a frame shorter than its header says.
- The buffer is the DVB one, not ATSC's: the export's 12,896 B is A/52 Annex G's
  for `stream_type` 0x87, and DVB AC-3 uses 5,696 B. Take the E-AC-3 value
  from ETSI TS 101 154, cite the clause, and add it to the schedule's
  per-PID admission and to the `tstd` model in `compliance.py`, which
  refuses DVB E-AC-3 today.
- Test: a synthetic multi-frame E-AC-3 PES exports one access unit per PES;
  an independent-plus-dependent fixture keeps both substreams on one
  interval (32 ms for six 48 kHz blocks, not 64); and a control in
  `tstd-controls.py` overflows B without the split.
