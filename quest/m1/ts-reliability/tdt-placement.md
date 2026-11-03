# [S] Clock-table revisions land at their media position

## Goal

Two semantic `moq export ts` of one broadcast, started at different times,
render the same packets from the moment they overlap, including around each
TDT/TOT revision, so a late or rejoining ST 2022-7 leg merges hitlessly.

## Plan

Decided 2026-11-03 (user, /quest-import of
[moq-dev/moq#7743](https://github.com/moq-dev/moq/issues/7743)).

- ✅ Place each revision by its import stamp. Not chosen: won't fix (drop the
  byte-identity promise for clock tables).
- Milestone: ✅ m1, in this epic. Not chosen: m2.

Byte identity across exports is already a contract (`export_test.rs:5601` at
976fece173, `doc/setup/redundancy.md`). Repeats of unchanged SI follow it:
`due()` slots them on a media-time grid. Clock-table revisions do not. At
`export.rs:3413`, `si.dirty` comes from when the snapshot group finished
arriving (`SiTrack::poll`, `:1401`), and `si_due` measures its 1 s floor from
the entry's own last emission. So placement depends on network arrival and
join time. The import already stamps every SI frame with the media PTS
(`si.rs:400`), but the export reads only the payload (`:1390`).

- Emit a revision at the first grid slot at or after its stamp. Hold one that
  arrives early until media reaches its stamp. Anchor the revision floor to
  the grid, not to the last emission.
- A revision that arrives after media has passed its stamp goes out on the
  next slot. That is the only case where exports can still differ, and it
  stays visible in the test.
- Extend the two-export fixture (`export_twice`) with TDT and TOT revisions
  and skewed SI arrival between the two exporters. Today it drains both after
  every write, which hides arrival order.

Public API: none. Wire: none.

## Closes

- [#7743](https://github.com/moq-dev/moq/issues/7743) - a late export reorders packets around each TDT/TOT revision
