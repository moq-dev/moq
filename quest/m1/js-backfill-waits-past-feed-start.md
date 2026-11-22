# [S] A JS front's backfill stops waiting on a copy whose feed starts past the gap

## Goal

A JS front's reader ends as soon as a plain cursor on the logical track would,
rather than once the budget's grace folds, when the only copy that might still
deliver a group in its window is a backfill on a copy whose answered START
lies past that group. Rust already ends: `TrackState::find_landable` clamps to
the copy's start (`self.start.key()`), while JS `#landing` in
`js/net/src/track.ts` drops its START bound once the demand lowers the floor,
which every backfill does.

## Plan

Found on [moq-dev/moq#9438](https://github.com/moq-dev/moq/pull/9438). Its JS
test `an unplaced time start waits below its time on a replaced copy` reads
group 6 and places the start, but `recvGroup` then stays pending for an hour
of mock time. `#retains` keeps the backfill on `tc` (`startTrack` at 9) as
`"wait"` for group 7, which the replaced copy declared skipped and `tc` can
never deliver. Add `expect(await sub.recvGroup()).toBeUndefined()` to that
test.

Fix it in JS to match Rust's rule. The lowered-floor exception in `#landing`
exists for fills (`owedRuns`), so keep it there and clamp only what
`mayLand` judges, unless the Rust side turns out to be the one that is wrong.
