# [M] A TS slot never repeats a PSI run back to back

## Goal

When keyframes on several video PIDs align in one slot, the export does not
lay identical PAT and PMT runs back to back, so the system buffer (512 bytes,
ISO 13818-1 2.4.2.3) never overflows at 25 Mb/s and up. Each send-ahead unit
still leads with its own tables as a tune-in point, and changed tables (a new
PMT version) always go out, in order. `compliance.py` grades the PAT and PMT
buffers too, so every TS Interop arm catches a regression.

## Plan

Found by Codex on #5207 (`schedule.rs`, repeated PSI groups). It exists on
`main` before #5207 too. Keeping each table once per slot was tried and broke
`aac_program_config_follows_each_table`, because send-ahead units need their
own tables.

At 25 Mb/s, three PAT and PMT pairs back to back deliver 564 B of PAT in about
301 µs, of which about 38 B drains at Rxsys = 1 Mb/s, peaking near 526 B. Two
renditions' 376 B fit.

- Start from the author's narrower diff on #5207
  (https://github.com/moq-dev/moq/pull/5207#discussion_r4239051134): drop a run
  only when the same flush would lay it right after an identical run, and pad
  the dropped packets back with nulls in a padded slot. `Slot::nulls` becomes
  `Option<usize>` so an unpadded slot gains none. A slot still opens with its
  clock packet and keeps its length, so no PCR moves.
- A new table version that bunches with the old one is not spaced out.
- Test against system-buffer fill with the T-STD model, not a packet count: a
  unit test runs laid slots' PAT and each PMT PID through its own 512-byte
  buffer drained at 1 Mb/s, with keyframes aligned at 25 Mb/s on three video
  PIDs (fails without the fix) and on two (passes either way). The AAC tune-in
  test still passes.
- `test/ts/compliance.py` gains a hard-severity system-buffer check for the PAT
  and every PMT PID: TBsys of 512 B drained at Rxsys, then Bsys, per ISO
  13818-1 2.4.2.3. Its `tstd` check models only elementary streams today. DVB SI
  (SDT, EIT) and SCTE-35 stay out, since ISO defines these buffers for
  program-specific information. Every existing TS arm still passes.

Public API: none. Wire: none.
