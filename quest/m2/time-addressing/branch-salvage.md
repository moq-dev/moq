# [S] Land what stands alone from the closed declared-ends branch

## Goal

The fixes and tests on closed
[moq-dev/moq#9192](https://github.com/moq-dev/moq/pull/9192)'s branch
(`quest/m2/time-addressing/consumers`, kept for reference) that do not depend
on reading GROUP_END's position land on main, each with a test that fails
without it.

## Plan

Decided 2026-11-19 (maintainer, closing #9192): salvage the TS export's PCR
PID fix, the Opus import's per-packet durations, and the end and loss tests
worth keeping. Research on the branch (head `20c13b1308`) found:

- PCR PID fix, commit `1ca5b60cac`: `revise()` in
  `rs/moq-mux/src/container/ts/export.rs` (main `:3405-3418`) keeps the PCR
  PID when the track on it is `cataloged`, not only `listed`. It uses only
  fields on main. It has no dedicated test: it surfaced under that commit's
  `revision_test.rs` harness change (AAC frames back to back), guarding
  `a_descriptor_change_revises_the_entry_and_keeps_the_clock`
  (`revision_test.rs:364`). Write a test that fails on main without the fix
  first; if main cannot hit the race, say so and drop it.
- Legacy test, commit `095cdea03a`:
  `an_old_publishers_marker_group_reads_as_an_end` in
  `rs/moq-mux/tests/legacy_audio.rs` writes a marker by hand on lite-05/06
  with main's APIs and should pass as is.
- Opus per-packet durations, commits `c4ad9643f3` and `7bb71b0710`:
  `decode()` in `rs/moq-mux/src/codec/opus/import.rs` sets `Frame.duration`
  from `packet_samples`. On main that gives `cut(None)` an exact
  `estimated_end`, so each Opus group finishes whole at its packet's end;
  on the branch that aging is why the WHEP tests changed
  (`rs/moq-webrtc/src/lib.rs` `flowing!`, and `cd42c40e49`). Recommended:
  land it only if something still reads the duration once
  [GROUP_END has no End](/quest/m2/time-addressing/group-end-whole.md), such
  as hang's duration marker or the HLS export, and say in the PR either way.

The rest of the branch (its end and loss tests, `read_breaks`, the HLS
`whole_end` reader, the webrtc ingest expectations) rests on reading
GROUP_END's position and stays behind.

Public API: none. Wire: none.
