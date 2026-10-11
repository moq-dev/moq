# [S] A TS slot never repeats a PSI run back to back

## Goal

When keyframes on several video PIDs align in one slot, the export does not
lay identical PAT and PMT runs back to back, so the system buffer (512 bytes,
ISO 13818-1 2.4.2.3) never overflows at 25 Mb/s and up. Each send-ahead unit
still leads with its own tables as a tune-in point, and changed tables (a new
PMT version) always go out, in order.

## Plan

Found by Codex on #5207 (`schedule.rs`, repeated PSI groups). It exists on
`main` before #5207 too. Keeping each table once per slot was tried and broke
`aac_program_config_follows_each_table`, because send-ahead units need their
own tables.

- Start from the author's narrower diff on #5207
  (https://github.com/moq-dev/moq/pull/5207#discussion_r4239051134): drop a run
  only when the same flush would lay it right after an identical run, and pad
  the dropped packets back with nulls in a padded slot.
- Test against system-buffer fill with the T-STD model, not a packet count:
  three or more aligned video keyframes at 25 Mb/s overflow today, and pass
  after the fix. The AAC tune-in test still passes.

Public API: none. Wire: none.
