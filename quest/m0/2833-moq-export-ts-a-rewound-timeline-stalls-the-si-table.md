# [M] moq export ts: a rewound timeline stalls SI tables, PCR, and pacing

## Goal

When the publisher rewinds its timeline, `moq export ts` keeps emitting SI
tables and PCR on cadence from the first frame of the new epoch, marks the
break with `discontinuity_indicator`, and re-anchors its pacer, instead of
going quiet until the media clock climbs back past where it had been.

## Plan

Three consumers in the exporter anchor on a media timestamp that only ever
moves forward, so a backwards jump freezes each of them for the rewound span:

- `due` in `rs/moq-mux/src/container/ts/export.rs` fires when a timestamp
  reaches a later repetition slot than the last emission's. PAT/PMT recover at
  the next video keyframe, but the SI PIDs (SDT, NIT) have no such escape and
  an audio-only program has none for PAT/PMT either.
- `write_pcr` treats a backwards slot as already served since #2967, so the
  PCR grid freezes rather than jumping.
- The stdout path in `rs/moq-cli/src/subscribe.rs` paces on `moq_mux::Pacer`,
  whose anchor also only moves forward, so a new epoch's frames are written on
  arrival with no smoothing until they recross the old base.

Firing whenever the slot changes is not the fix: video is in decode order, so
a reordered B-frame PTS steps backwards constantly, and that was measured at
25x the intended PSI rate. A rewind and a reorder are separable without a
threshold, because `container::Consumer::discontinuity()` already counts
exactly the rewinds that matter, with group-level context. `ExportSource`
holds that consumer and never reads the counter, and `discontinuity_indicator`
is hardcoded false at the exporter's one adaptation-field site.

- Forward the counter from `ExportSource` to `ts::Export`. When it changes:
  clear `last_psi`, `last_si`, and `last_pcr` so the next frame is due, and
  carry a pending `discontinuity_indicator` that the next packet on the PCR
  PID sets exactly once. That packet is the standalone PCR-only one from
  `pcr_packet`, whose flags byte is hardcoded today, not the PES adaptation
  field in `write_pes`; an audio-only program never reaches the latter.
- `Delivery` in the cli export path calls `Pacer::hurry` on the same signal;
  that function exists for this and is only reached from an overshoot today.
- Tests: a rewound-timeline fixture asserts SI re-emitted within one interval,
  PCR resuming on the first new frame, the indicator set exactly once, and the
  reordered-B-frame case still emitting tables at the intended rate; the
  `test/ts` TSDuck analysis stays clean.

## Closes

- [#2833](https://github.com/moq-dev/moq/issues/2833) - close this issue when the quest finishes
