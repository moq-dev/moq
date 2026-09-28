# [S] Splice edge cases

## Goal

Three spliced-track cases in `rs/moq-net` stop losing or mis-judging groups,
each with a regression test that fails before its fix:

- A group whose successor segment's first servable group is still unstamped
  keeps an unbounded reach, instead of borrowing a later segment's start and
  being skipped or ended with `Error::Old`.
- A reader still draining a pruned segment has its boundary group judged
  against the later segments, so a stale boundary group is skipped like any
  other.
- A reader draining a warm head keeps it when the upstream fails over again
  before the track parks.

## Plan

Codex raised all three on https://github.com/moq-dev/moq/pull/4103 and
https://github.com/moq-dev/moq/pull/4104 after the fixes there landed; none
was answered.

- `served_start` in `rs/moq-net/src/model/resume.rs` says an unstamped group
  stops the search, but `find_map` reads a segment's `None` (no group, or its
  first group has no frame yet) as a miss and moves on. The two cases need
  to be distinguishable, as the per-track `served_start` in
  `rs/moq-net/src/model/track.rs` already treats them.
- `ResumeState::successor` finds the cursor's segment by id, so a segment
  `prune` removed while a reader still drains it yields no successor at all.
  Resolve it from the boundary and the remaining later segments instead.
- An ordinary takeover in `rs/moq-net/src/model/origin.rs` (`Action::Splice`
  with no warm copy) overwrites `io.head` with `None`. Dropping the
  `WarmGroup` aborts the cached head that a reader resumed from an earlier
  park may still be reading, and the next park cannot rebuild the full group.
  Keep the existing head when the splice has no new one to take.

These are independent; one PR is fine since they share the splice tests.
