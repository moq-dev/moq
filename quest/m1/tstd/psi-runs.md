# [L] TS export paces PSI within the system buffer

## Goal

The export's PAT and PMT never overflow the program's system transport buffer
(TBsys, 512 bytes drained at Rxsys = 1 Mb/s, ISO 13818-1 2.4.2.3) at any mux
rate. Identical runs that the same flush would lay back to back go out once.
Distinct tune-in runs (each send-ahead unit leads with its own tables) and
changed tables (a new PMT version) are paced, never dropped, and changed tables
go out in order. `compliance.py` grades TBsys too, so every TS Interop arm
catches a regression.

## Plan

Found by Codex on #5207 (`schedule.rs`, repeated PSI groups). It exists on
`main` before #5207 too. Keeping each table once per slot was tried and broke
`aac_program_config_follows_each_table`, because send-ahead units need their
own tables.

The PAT and the PMT share one TBsys: it takes PIDs 0 to 3 and the selected
program's PMT PID (ISO 13818-1 2.4.2.3). One PAT and PMT pair (376 B) fits. At
25 Mb/s, two pairs back to back deliver 752 B in about 180 µs, of which about
23 B drains, peaking near 729 B; three peak near 1090 B. Two distinct pairs fit
only about 1.9 ms apart, at any mux rate.

- Start from the author's narrower diff on #5207
  (https://github.com/moq-dev/moq/pull/5207#discussion_r4239051134): drop a run
  only when the same flush would lay it right after an identical run, and pad
  the dropped packets back with nulls in a padded slot. `Slot::nulls` becomes
  `Option<usize>` so an unpadded slot gains none. A slot still opens with its
  clock packet and keeps its length, so no PCR moves. #5304 implements this
  step and stays a draft until pacing lands.
- Then pace what suppression leaves: a distinct tune-in run or a changed
  version goes out only once TBsys has drained enough to take it, within the
  slot budget and the units' decode deadlines. #5304 found equal-weight aligned
  units on four and eight video PIDs still overflow after suppression.
- #5220 (multi-packet PMT) adds PMT pacing at the system-buffer rate, since a
  PMT section of up to 1,024 B spans several packets and can exceed 512 B alone.
  Build on that pacing rather than adding a second one, or own it here if #5220
  lands without it. Pace every PID that enters TBsys as one lane: the PAT, the
  PMT, and any `mpegts.si` passthrough on PIDs 1 to 3, which the scheduler gives
  its own `Buffer::SYSTEM` lane today.
- Test against TBsys fill with the T-STD model, not a packet count: a unit test
  runs laid slots' PAT and PMT packets through one shared 512-byte TBsys
  drained at 1 Mb/s, with keyframes aligned at 25 Mb/s on two and three video
  PIDs (both fail without the fix) and on one (passes either way). Add a
  changed-version regression and #5304's distinct-run cases. The AAC tune-in
  test still passes.
- `test/ts/compliance.py` gains a hard-severity system-buffer check: for each
  program, one 512 B TBsys fed by PIDs 0 to 3 and that program's PMT PID,
  drained at Rxsys into Bsys. Its `tstd` check models only elementary streams
  today. Other SI (NIT, SDT, EIT) and SCTE-35 never enter TBsys. Every existing
  TS arm still passes.

Public API: none. Wire: none.
