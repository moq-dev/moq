# [M] MPEG-TS lane converges with MSFTS ES-level carriage

## Goal

A subscriber author can map this repository's demultiplexed TS lane
(access units, Hang catalog `mpegts` section) onto MSFTS ES-level carriage
without guessing. Transporting TS verbatim is a non-goal.

## Plan

Decided in the 2026-09-30 audit: moved from m4 and the msfts#33 gate removed.
msfts#33 closed on 2026-09-28, answered by msfts#36 (merged 2026-09-24), and
egress moved to msfts#37 (closed). Two differences remain:

- **Program tables.** MSFTS carries them in tracks; here they live in the
  Hang catalog's `mpegts` section. Decide whether to publish a mapping from
  the catalog section to MSFTS's table tracks, or change either side.
- **ES units.** MSFTS es-units carry the whole PES packet; here they carry the
  payload plus `stream_id`. Decide whether the PES header fields we drop
  matter to a subscriber, and converge or document the mapping.

Update `drafts/draft-lcurley-moq-mpegts.md` and `doc/concept` with whatever
lands. Open TS PRs [#4577](https://github.com/moq-dev/moq/pull/4577) (per-ES
access units at export), [#4579](https://github.com/moq-dev/moq/pull/4579)
(export on the mux rate), and [#4580](https://github.com/moq-dev/moq/pull/4580)
(per-program SI) touch the same area; land or rebase on them first.
