# [M] Resume duplicates

## Goal

An ordered resume reader skips a route copy's groups that another copy
already delivered, without consuming them. `origin/resume_overlap/sequence`
costs about one cheap skip per duplicate again.

## Plan

Implemented in [#6292](https://github.com/moq-dev/moq/pull/6292):

- Skip superseded copies whose eligible range is already delivered, retaining
  them for recovery of groups still being read.
- Advance ordered copy cursors without delivery, metering, or cache refresh;
  propagate the floor through nested readers and pending copies.
- Keep the existing reader, publisher-copy, and group benchmark sweeps, and
  add overlapping copies whose serving replacement awaits its first group.
- At 32 copies and 64 groups, the covered-copy guard removes about 74% of
  drain instructions in both modes. While the new copy waits, ordered seek
  removes 58% versus the guard alone; arrival is unchanged.

Source work is complete. Automatic approval review rejected removing this
completed quest as unauthorized abandonment. Retire the quest and its links
once that narrow removal is explicitly approved; do not bypass the rejection.
The draft PR records the decision and final validation.
