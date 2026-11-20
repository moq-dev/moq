# [L] A resumed reader's time start below its copy's resume point starts at the group containing it

## Goal

A resume reader (`rs/moq-net/src/model/resume.rs`, `js/net/src/resume.ts`) whose range starts at a time below the serving copy's resume point starts at the group containing that time, as a plain reader of the route would, not at a group key equal to the time. Rust and JS match.

## Plan

The serving copy is asked from its floor (`clamped` raises a time start to `max(time, floor)`), so its resolution says nothing of which group the original time names. [moq-dev/moq#9224](https://github.com/moq-dev/moq/pull/9224) stopped placing the window at the floor (`unraised` in both languages). It now places it at the time itself, as a group key. That is still wrong when no group starts exactly there: an arrival reader resumed at group 8 that asks from time 7, on a route holding groups 6 and 8, places the start at 7. `#backfillGaps` (Rust `backfill_gaps`) then backfills from 7, which excludes group 6, the group holding time 7 and possibly the seek's keyframe. Codex raised this on #9224's review.

- Recommended: a copy whose floor raised a range's time start leaves that start unplaced. The reader then asks the route for a backfill below the resume point that carries the original time anchor unclamped, and the backfill's resolution places the start, as the first copy to place a range does today. Groups the serving copy offers at or above its floor keep flowing while the start waits, since they lie in range wherever it resolves.
- Check that a window waiting on such a start never hands out a group below where it will resolve, from a replaced copy in particular.
- The resume models' oracles (`resume_model.rs`, `resume.model.test.ts`) require only groups at or above a range's start time (`within`), so no sweep catches this. Teach both oracles the group containing a time start, from what the routes wrote, then add the case above as a regression in both.

Public API: none. Wire: none.

## Related

- [moq-dev/moq#9172](https://github.com/moq-dev/moq/pull/9172) - adds `quest/m1/time-start-asks-upstream.md`, the relay copy's version: placing a time start only where it can prove upstream's answer
