# [M] TS export keeps PSI within the system buffers

## Goal

Padded `moq export ts` output keeps the program's system buffers within
ISO 13818-1 2.4.2.3 at every mux rate: TBsys (512 B at Rxsys = 1 Mb/s, shared
by PIDs 0-3 and the PMT PID) and Bsys (1,536 B drained at
Rsys = max(80 kb/s, mux rate / 500)). Distinct tune-in runs and changed table
versions are paced, never dropped, and changed versions go out in order.
`compliance.py` grades both buffers, so every TS Interop arm catches a
regression. Unpadded output has no mux rate and makes no system-buffer claim;
say so in the profile report.

## Plan

Decided (2026-10-13), from #5304: adjacent-copy suppression is dropped. It
would only reach unpadded output, which makes no buffer claim, and #5306's
padded planner already passes #5304's 3/4/8-PID regression with every tune-in
copy kept.

- Build on #5306's padded planner, which paces TBsys at absolute positions
  shared by tables, nulls and media. t0ms found on #5304 (a real DVB recording)
  that table and padding positions laid ahead of a capped video lane, outside
  its interleave count, compress the video and overrun its TB. Any further
  pacing must stay inside that planner's positions, never a pre-pass that moves
  or pads tables outside it. Confirm with t0ms's replay against the planner.
- Add Bsys beside TBsys in the planner. Settle first, from 2.4.2.3's text, what
  enters Bsys (whole 188 B packets, payloads with stuffing, or section bytes):
  for single-packet tables it decides whether Bsys can overflow at all. A probe on #5306 (`a5640abee`) counting
  full payloads peaked near 1.35 KB at 4 aligned video PIDs and 2.3-2.7 KB
  at 8.
- Open: when Bsys pacing would push a table past its unit's decode deadline,
  fail loud like any missed deadline, or allow dropping a copy identical to one
  still queued. Recommend failing loud unless a real source hits it.
- Pace `mpegts.si` passthrough on PIDs 1-3 in the same TBsys and Bsys; the
  planner's `transport_buffer` maps only PID 0 and the PMT to TBsys.
- Delete the round-robin `SystemBuffer` stall in `Slot::layout` (#5220), which
  only unit tests reach once the planner handles padded output, or move its
  cases to the planner.
- Tests: planner unit tests with 8 aligned PIDs, a multi-packet PMT, a PID 1-3
  passthrough packet beside a PAT/PMT pair, a changed PMT version going out in
  order, and a burst spanning several slots that fits TBsys but overflows Bsys.
  The AAC tune-in test still passes. `test/ts/compliance.py` gains a hard-severity check
  per program: one TBsys fed by PIDs 0-3 and the PMT PID, draining into one
  Bsys, both checked for overflow, and TBsys emptying at least once a second
  (2.4.2.6), each with a negative control. Every existing TS arm still passes.

Public API: none. Wire: none.
