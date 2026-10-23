# [M] Keep the old tail on both 2022-7 legs across a restart

## Goal

Two `moq export ts --sync` legs that see a timeline restart (a marker or
source restart that opens a new generation) both send the old timeline's
tail up to the restart, so an ST 2022-7 receiver merging them has no hole
there. Ordinary skips already cost nothing, because the other leg's copies
fill in.

## Plan

Found in [moq-dev/moq#6304](https://github.com/moq-dev/moq/pull/6304)
(RTP egress), which this builds on. Each leg's lane forgets the positions it
already holds: `Lane::reset` and `Lane::jump` in
`rs/moq-mux/src/container/ts/lane.rs` clear `from` when the restart is read.
The old tail is laid about one delay later, so all of it is marked unsure and
withheld, costing roughly 0.5 to 1 s of output on both legs at a video and
audio restart.

- Old-timeline lane knowledge stays valid until the rewind that adopts the
  new generation.
- Keeping `[from, end)` known across a skip jump or a partial group is sound.
  A restart is not obviously sound: `reset()` sets `end` to 0, and
  new-timeline positions can land inside the old range when a restart opens
  no new generation. Settle that first: either every restart opens a
  generation, or lane knowledge is kept per timeline.
- Reproduce with a mocked-time two-leg test of a video and audio restart that
  asserts the merged legs cover the old tail. Extend
  `an_rtp_leg_sends_its_paced_tail_before_a_restart` in
  `export_timing_test.rs` from that PR if it has merged.

Public API: none.
