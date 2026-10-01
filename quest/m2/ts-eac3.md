# [S] DVB E-AC-3 gets a buffer model in TS export

## Goal

`moq export ts` splits DVB E-AC-3 private-data PES one sync frame per PES,
schedules each frame against the 13818-1 E-AC-3 buffer (12,896 B), and
`test/ts/compliance.py` grades it. That's the same treatment DVB AC-3 got in
the fixed-delay export (#4645).

## Plan

Found in #4645: E-AC-3 carried as DVB private data still goes out unsplit,
with no buffer model, so a multi-frame PES can overflow B.

- Split it the way #4645 splits DVB AC-3. Read frame sizes from each E-AC-3
  sync frame's `frmsiz`, and drop a frame shorter than its header says.
- Add E-AC-3 to the schedule's per-PID admission (TB at 2 Mb/s, B at
  12,896 B) and to the `tstd` model in `compliance.py`.
- Test: a synthetic multi-frame E-AC-3 PES exports one frame per PES, and a
  control in `tstd-controls.py` overflows B without the split.

## Required

- [T-STD TS export](/quest/m1/tstd/README.md) - the per-PID schedule this joins
