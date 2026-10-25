# [S] TS import holds only what its source keeps repeating

## Goal

A TS import's retained state is bounded by what its source repeats, not by
how long it runs or by a constant that refuses. SI arriving without program
clock progress is bounded too.

## Plan

SI already retires when not repeated within its table's maximum interval on
the accepted program clock (`si::Entry::expire`, 30 s for unknown tables), so
an input stall retires nothing. A stream revision already costs O(1) state
once its route has ended: the route's track name is minted per route, and
`catalog::Producer::retire` finishes its timeline and drops it from the
registry and the `archive` entry.

Decided 2026-10-22 by the maintainer:

- Rejected: one byte budget shared by SI, PES and staged media, wall-clock
  expiry, or any constant that only refuses the source.

Open, for the maintainer:

- **SI without clock progress.** Standalone SI is accepted before any PCR and
  while PCR stalls, and its sections are not charged to the PES staging
  owner, so a source sending fresh EIT identities with a frozen clock grows
  `Entry::pending` and `Entry::active` forever. Choose an admission or
  ownership contract for it, or narrow the Goal to sources with clock
  progress. Candidate: admit SI on the program clock as whole PES are (drop
  and count it unplaced before the first PCR), and age it on the input
  position extrapolated at the measured multiplex rate while PCR stalls.

Already bounded, so not per-revision growth: catalog rendition reservations
(released on drop), `retired_stats`, damage and liveness (keyed by PID), and
`si::Capture::entries` (four SI PIDs by 256 table IDs, kept for track
reappearance).

Tests: the chosen no-clock SI contract.

Public API: none expected. Wire: none.
