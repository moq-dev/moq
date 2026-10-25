# [M] TS import holds only what its source keeps repeating

## Goal

A TS import's retained state is bounded by what its source repeats, not by
how long it runs or by a constant that refuses. SI arriving without program
clock progress is bounded too, and a stream revision costs O(1) state once
its route has ended, so a feed revising its PMT indefinitely runs
indefinitely.

## Plan

SI already retires when not repeated within its table's maximum interval on
the accepted program clock (`si::Entry::expire`, 30 s for unknown tables), so
an input stall retires nothing. Two owners remain unbounded.

Decided 2026-10-22 by the maintainer:

- Replace `Import::MAX_REVISIONS` by making an ended route cost O(1): mint
  fresh names from a counter so reserving them is a high-water mark, and
  drop the retired route's entries. Keep the guard until every owner is
  bounded.
- Rejected: one byte budget shared by SI, PES and staged media, wall-clock
  expiry, or any constant that only refuses the source.

Open, for the maintainer:

- **SI without clock progress.** Standalone SI is accepted before any PCR and
  while PCR stalls, and its sections are not charged to the PES staging
  owner, so a source sending fresh EIT identities with a frozen clock grows
  `Entry::pending` forever. Choose an admission or ownership contract for it,
  or narrow the Goal to sources with clock progress.
- **Retired route names and archives.** `BroadcastState::unique`
  (`rs/moq-net/src/model/broadcast.rs`) already is a high-water counter and its track lookup prunes dead entries, but
  `BroadcastState::sequences` keeps every written name's sequence and
  timestamp bounds to enforce explicit same-name continuation. MKV, fMP4,
  C/FFI and GStreamer callers keep `unique_name` strings for a later
  create/reserve, so a missing name below the counter may be a delayed first
  creation rather than a retired one. `moq-mux`'s `timeline::Registry::tracks` likewise
  keeps finished recorders' outputs, numbering and `archive.timelines`
  discovery after `Recorder` drops. Decide when each may retire while held
  readers and same-name re-enrollment stay valid; inferring ownership from a
  name prefix is not enough.

Already bounded, so not per-revision growth: catalog rendition reservations
(released on drop), `retired_stats`, damage and liveness (keyed by PID), and
`si::Capture::entries` (four SI PIDs by 256 table IDs, kept for track
reappearance).

Tests: a fixture revising its PMT past 65,536 times keeps running with flat
sequence, timeline, catalog and route state, with name continuation and
delayed creation still covered; the chosen no-clock SI contract.

Public API: none expected. Wire: none.
