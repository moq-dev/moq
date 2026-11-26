# [M] No resume or relay route mutant survives

## Goal

Nightly's mutants shards report no survivors in
`rs/moq-net/src/model/resume.rs` or `rs/moq-net/src/route/relay.rs`.

## Plan

[moq-dev/moq#9540](https://github.com/moq-dev/moq/pull/9540) killed or excluded the
survivors of Nightly run [45242597151](https://github.com/moq-dev/moq/actions/runs/45242597151),
the rotation slices since 2026-11-19, and the in-diff set tonight's Nightly would test,
all rerun on main at [moq-dev/moq#9531](https://github.com/moq-dev/moq/pull/9531). The
ten below are left: each was probed by hand and none was staged, nor proven equivalent,
so none is excluded. Kill each with a unit test or a model regression, or exclude it in
`.cargo/mutants.toml` with the proof. cargo-mutants 27.1 ignores `exclude_re` for
"delete field" mutants, so those must be killed or rewritten away.

- `Copy::asked_for` `key < end` to `<=`: the extra copy counted, one whose end is the
  asking key, matters only before an ask copy exists, since `let_go` keeps the one asked
  furthest. Where it could, `backfill` widens it rather than asking afresh from the key, so
  the route is asked from a lower floor. Needs a copy ending exactly on a kept-out key
  still owed.
- `Reader::owed` `copy.start(start) <= key` to `>`: changes `owed` only for a key a copy
  still read holds cached, deliverable and undelivered at `let_go`. Every such holder found
  is covered another way (a replaced non-fill copy removes its fed range, the serving copy
  starts above, a fill waits through its unread group). Needs a second fill, from another
  route, holding a key the first covers while the local cap holds it back.
- `Reader::poll_first` `!copy.replaced && Some(copy.generation) == self.generation`, `&&`
  to `||`: also counts a late backfill of the serving route, which only reruns
  `backfill_gaps` (a no-op, or a reset of that backfill's budget), and a last copy of an old
  generation left while the route has no copy. That one reading late with a floor may ask
  its old route for a new backfill: the likelier kill.
- `Reader::backfill` `copy.backfill |= raised` to `&=`: a reused backfill widened with a
  raised start (A received holds 6 and 8, reader `from_times(&[6, 9])`; switch to B, to C
  with group 10, back to B) showed no difference, since the flag only matters for a group
  under the backfill's floor holding the time. Needs one group spanning that floor, such as
  times 6 to 10.
- `Reader::backfill_gaps` `start` closure, `replaced.is_none_or(|replaced| start > replaced)`:
  the guard to `true` and to `false`, and `>` to `==`, `<`, `>=`. The backfill's floor
  barely matters, since its wire ranges clip to the reader's own, and a lower gap left
  unasked by one switch was asked by the switch before it. Lead: a failed or late copy sets
  `Resumed::Route(None, {})`, where the closure compares a raw time start against a window
  placed lower, at the group holding it.
- `Reader::backfill_gaps` `time < resumed` to `<=`: flips only the backfill's `raised` flag,
  which matters when a sparse-key group under the backfill's floor holds its start time.

The late-group lead from the 2026-11-22 run did not reproduce in eight setups;
`a_start_moved_to_the_resume_point_takes_no_late_group_below_it` pins it.

Public API: none. Wire: none.

## Related

- [Resume model misses a group](/quest/m0/nightly/resume-model-missing-group.md) - a seed the
  model fails on, found while probing these
