# [M] TS import holds only what its source keeps repeating

## Goal

A TS import's retained state is bounded by what its source repeats, not by
how long it runs or by a constant that refuses. Incomplete SI generations
and SI sub-tables the source stopped sending retire, and a stream revision
costs O(1) state once its route has ended, so a feed revising its PMT
indefinitely runs indefinitely.

## Plan

Found in the 2026-10-22 audit:

- `si::Entry::pending` (`rs/moq-mux/src/container/ts/si.rs`) only clears
  when the same identity commits, and `active` never retires an identity.
  EIT identities come from the stream, so 4,096 distinct incomplete EIT
  tables all stayed pending.
- `Import::MAX_REVISIONS` (`rs/moq-mux/src/container/ts/import.rs`) ends an
  import after 65,536 revisions, because each reopened route reserves a
  fresh track name for the broadcast's life.

Decided 2026-10-22 by the maintainer:

- An SI sub-table, pending or active, that the source has not repeated
  within its table's maximum repetition interval (ETSI EN 300 468 / TR 101
  211, measured on the program clock so an input stall retires nothing)
  retires; an active one's retirement cuts a snapshot without it. A
  `table_id` `catalog::si_interval` does not know has no table maximum, so
  it retires on the longest one EN 300 468 sets for any SI table (30 s, EIT
  schedule other and TDT/TOT): every conformant table repeats within it.
- Replace `MAX_REVISIONS` by making an ended route cost O(1): mint fresh
  names from a counter so reserving them is a high-water mark, and drop the
  retired route's entries. Find every per-revision store first (catalog
  producer, broadcast reservations, `retired_stats`).
- Rejected: one byte budget shared by SI, PES and staged media. It is
  another runtime constant; the source's repetition is the bound by design.

Tests: incomplete EIT generations with distinct identities retire after
their interval, and so do unknown long-form `table_id`s; a sub-table the source stops sending leaves the snapshot; a
stall in input retires nothing; a fixture revising its PMT past 65,536 times
keeps running with flat retained state.

Public API: none. Wire: none.
