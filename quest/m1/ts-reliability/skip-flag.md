# [S] A skipped group flags the next PCR

## Goal

When a semantic `moq export ts` skips media within one broadcast instance,
the next PCR carries `discontinuity_indicator`, so a decoder or groomer sees
a marked break instead of an unflagged forward jump in PCR and PTS.

## Plan

Decided 2026-11-03 (user, /quest-import of
[moq-dev/moq#7741](https://github.com/moq-dev/moq/issues/7741)).

- ✅ Flag the PCR only, with no PMT version bump: the program is unchanged,
  only its clock jumped. Not chosen: mark it like a `--stitch` switch (every
  PID flagged and a new PMT version), or won't fix.
- Milestone: ✅ m1, in this epic. Not chosen: m2.

Today a skip (`Item::Gap`, `container/source.rs:601` at 976fece173) reaches
`ts::Export` only as a count that drives decodable-start recovery
(`export.rs:447`). PCR keeps following media time, so it jumps forward with
nothing marked. Our own importer counts that jump as a
`PCR_discontinuity_indicator_error` (`health.rs:261`).

- Set `pcr_discontinuity` for the first PCR after a skip, as `rewind()`
  (`export.rs:2852`) already does, without touching `version` or
  `pat_version`. Decide where the flag lands relative to the backfilled slots
  (`schedule.rs` `BACKFILL`), so no unflagged jump over 100 ms remains.
- Rewrite `a_skipped_group_keeps_the_clock` (`export_test.rs:8256`) and the
  same-instance handoff test (`:10151`) to expect one flagged PCR and no
  table version change. Feed the output back through the importer's `Health`
  and assert no `PCR_discontinuity_indicator_error`.
- The raw `--passthrough` lane stays unflagged: under this epic's rule, loss
  in the fallback never rewrites packets.
- Update the TS guide's coverage row for loss in the semantic lane.

Public API: none. Wire: none (output TS only).

## Closes

- [#7741](https://github.com/moq-dev/moq/issues/7741) - a skip within one instance reaches the wire unflagged
