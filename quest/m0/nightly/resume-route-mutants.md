# [M] No resume or relay route mutant survives

## Goal

Nightly's mutants shards report no survivors in
`rs/moq-net/src/model/resume.rs` or `rs/moq-net/src/route/relay.rs`.

## Plan

[moq-dev/moq#9619](https://github.com/moq-dev/moq/pull/9619) killed, rewrote away, or
excluded the rest of the quest's survivors and the nine new ones from Nightly run
[45667362304](https://github.com/moq-dev/moq/actions/runs/45667362304). These three are
left: each was probed by hand, and none was staged or proven equivalent, so none is
excluded. Kill each with a unit test or a model regression, or exclude it in
`.cargo/mutants.toml` with the proof.

- `Copy::asked_for` `key < end` to `<=`: the extra copy counted, one whose end is the
  asking key, matters only before an ask copy exists, since `let_go` keeps the one asked
  furthest. Where it can, `backfill` widens that copy instead of asking again from the key,
  so the route is asked from a lower floor (`asked_for` in the tests sees it). Needs a
  backfill of the serving route ending exactly on a kept-out key that is still owed, with
  every key below it delivered or skipped. Neither a `set_range` backfill, which ends at the
  old local floor, nor a returning route's, whose next `backfill_gaps` widens it in the same
  poll, got there.
- `Reader::backfill` `copy.backfill |= raised` to `&=`: a reused backfill widened by a raised
  ask, or a raised one widened by a plain ask. The flag only matters for a group under the
  copy's floor and at or above the local floor. A `set_range` backfill's floor is the local
  floor, and a lower floor asks a new backfill that covers the group.
- `Reader::backfill_gaps` `time < resumed` to `<=`: flips `raised` only for a range whose
  time start is the serving copy's resume point, a group's own key, so no group under it
  holds that time. The flagged backfill could then hand out a cached window group under its
  floor. In `[from, start)` that is a group a replaced copy drains. Under an edit's `from`,
  it would be a group an older range lacks that landed on the route after its backfill was
  let go. That can't happen in practice, because `ledger::grace` equals the budget: a group
  landing that late is already past the budget.

Public API: none. Wire: none.

## Related

- [Resume model misses a group](/quest/m0/nightly/resume-model-missing-group.md) - a seed the
  model fails on, found while probing these
