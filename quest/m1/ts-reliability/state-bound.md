# [M] TS import holds only what its source keeps repeating

## Goal

A TS import's retained state is bounded by what its source repeats, not by
how long it runs or by a constant that refuses. Incomplete SI generations
and SI sub-tables the source stopped sending retire, and a stream revision
costs O(1) state once its route has ended, so a feed revising its PMT
indefinitely runs indefinitely.

## Plan

The program-clock SI retirement portion is implemented in this checkpoint:

- Active and pending generations renew their own deadline on repetition.
  The table's `catalog::si_interval` supplies the maximum; unknown tables use
  the approved 30 s maximum. Retirement uses accepted PCR intervals already
  accumulated by `Import::liveness`, not media PTS or host time. Clock wrap,
  reset and rejected jumps do not invent elapsed time.
- A deadline index holds one entry per retained generation, replacing the old
  deadline on renewal. Expiry visits due entries rather than scanning every
  identity on every packet. A commit clears only its identity's five-bit
  versions. Active expiry publishes a reduced or empty snapshot immediately,
  bypassing ordinary host-clock debounce and retracting empty catalog entries.
  Readers holding earlier snapshots remain valid.
- Tests cover 4,096 incomplete EIT and unknown identities, per-version renewal,
  active renewal/retraction/reappearance, input stalls, and clock boundaries.
  The SI renewal benchmark sweeps retained identities and touched identities.

The full Goal remains unfinished. Preserve the maintainer's 2026-10-22 choices:

- Retire pending or active SI not repeated within its table maximum on the
  program clock. An input stall retires nothing. Unknown tables use 30 s,
  the longest DVB SI maximum.
- Replace `MAX_REVISIONS` with retired-route ownership bounded independently
  of lifetime revisions, using fresh-name high-water allocation and releasing
  retired entries. Do not remove the guard before proving every owner bounded.
- Rejected: a shared SI/PES/staged-media byte budget, wall-clock expiry, or
  an arbitrary constant that only refuses the source.

### Remaining contract choices

Continued new SI identities without accepted PCR progress are not bounded by
repetition age. Standalone SI can arrive before the program clock exists, and
its retained sections are not charged to the PES staging owner. The test with
1,023 new incomplete identities during a frozen clock deliberately preserves
all of them. Establish the admission/ownership contract for this input before
claiming a full SI bound; do not silently age it on host time or count packets.

The retired-route inventory also found lifetime owners that cannot simply be
forgotten privately under current contracts:

- `BroadcastState::unique` already is one high-water counter, and its track
  lookup already prunes dead weak entries. `sequences` intentionally retains
  every written name's sequence and timestamp bounds. `unique_name` returns
  a `String` that real MKV/fMP4, C/FFI and GStreamer consumers later pass to
  create/reserve, and explicit same-name continuation remains supported.
  Treating every missing name below the counter as retired would reject a
  delayed first creation; forgetting its bounds would permit content reuse.
- `timeline::Registry::tracks` retains finished recorders' timeline outputs,
  numbering and discovery. `Recorder::drop` only flushes and marks recording
  inactive; catalog `archive.timelines` continues advertising them. Define when
  that archive ownership may retire while preserving existing readers and
  same-name re-enrollment before removing those entries.
- Catalog rendition reservations already leave on their owner's drop.
  `retired_stats`, damage counters and liveness aggregate by finite PID, not
  by revision. They are not per-revision growth.

Recommend explicit ownership for fresh-name reservations together with an
archive retirement policy that keeps readers' current snapshots valid.
Keeping arbitrary explicit-name continuation still requires its history.
The alternative is a reviewed change to name reuse/archive availability;
string-prefix inference is insufficient. No public shape is chosen here.

Remaining validation: indefinite PMT revision churn past 65,536 with measured
flat sequence/timeline/catalog/route ownership, plus the selected no-PCR SI
admission contract. Existing name continuation and delayed creation consumers
must remain covered. `MAX_REVISIONS` stays until these conditions are met.

Public API: none in this checkpoint. Wire: none.
