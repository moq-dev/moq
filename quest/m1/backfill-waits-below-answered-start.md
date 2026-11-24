# [S] Rust and JS agree on whether a backfill waits below a copy's answered START

## Goal

A front's reader waits the same way in Rust and JS when the only copy that might
still deliver a group in its window is a backfill on a copy whose answered
START lies past that group. Today they differ:

- Rust stops waiting. `TrackState::find_landable` (`rs/moq-net/src/model/track.rs`)
  clamps to the copy's start (`self.start.key()`) whatever the floor.
- JS keeps waiting until the budget's grace folds. `#landing` in
  `js/net/src/track.ts` applies the START bound only while the demand keeps the
  floor START answered. Every backfill lowers it, raised time starts included:
  `#backfill` in `js/net/src/resume.ts` asks only for groups under the serving
  copy's resume point, and returns early when there are none.

## Plan

Decided 2026-11-24 (maintainer, /quest-run pick on moq-dev/moq#9438): ✅ keep this quest (rejected: drop).

Found on [moq-dev/moq#9438](https://github.com/moq-dev/moq/pull/9438). Its JS
test `an unplaced time start waits below its time on a replaced copy` reads
group 6 and places the start, but `recvGroup` then stays pending for an hour of
mock time. `#retains` keeps the backfill on `tc` (`startTrack` at 9) as
`"wait"` for group 7. The replaced copy declared 7 skipped, and `tc` was asked
from below its START. The matching Rust test ends at once.

First decide which side is right, from what a lowered floor asks of the wire.
If it asks the feed for the groups below START, as the JS comment says, those
groups may still land, so JS is right and the Rust clamp can end a read early.
Otherwise Rust is right. Fix the wrong side, keep the lowered-floor exception
`owedRuns` relies on for fills, and give the JS test and its Rust twin the same
end assertion (`recvGroup` resolving, or `poll_recv_group` ready).
