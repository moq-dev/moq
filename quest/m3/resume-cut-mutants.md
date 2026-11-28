# [M] Nightly mutants pass on resume.rs's back-cut recovery

## Goal

The Nightly `mutants` shards report no missed mutant in
`rs/moq-net/src/model/resume.rs`'s back-cut (`Cause::Cut`) recovery and the
guards around it, so a red shard there means a new gap. Each surviving
mutant is killed by a test or its code is deleted or excluded with a written
reason.

## Plan

Found by the triage of Nightly
[46038678705](https://github.com/moq-dev/moq/actions/runs/46038678705)
(`f7a41c3ea5`): 19 mutants survived on `main` (rerun at `f7684041f5` with
`just rs mutants --re ...`). The three easy ones are killed by this triage's
test PR; this quest owns the rest. Lines are at `f7684041f5`.

Decided 2026-11-28 (maintainer, /quest-plan): ✅ kill the easy mutants
inline and plan the rest in m3 (rejected: one m0 quest for all 19; easy
tests only, excluding the Cut arms). The back-cut path shipped untested, but
it is a recovery corner rather than the cut's blocker.

Most survivors came from [moq-dev/moq#9687](https://github.com/moq-dev/moq/pull/9687),
which added `Cause::Cut` (a Position End back-cut while switching routes)
with almost no tests. A harness that caps copy A mid-group at frame k during
a switch should cover these in one test file:

- `Recover::wants` (~4109): deleting the `Cut` arm. A cut arriving after a
  Stalled poll already moved past a switch-back must still recover.
- `Recover::poll` (~4309): guard `staged.is_none() || done` to true, false, and
  `&&`. A cut with no serving copy returns `Evicted` with nothing staged or the
  front done, and stays Pending while a route is staged.
- `Recover::poll` (~4339): `Cut && staged && !done && Err` to `||`, and deleting
  `!`. A cut while a route is staged and the serving route refuses defers to
  Pending.
- `Recover::poll` (~4346): deleting the `Cut` fallback arm. A cut with the front
  done and the serving route failed ends in `Evicted`, not `NotFound`.
- `held` (~4386) and `poll_serving` (~4413): `index == index && !past_cut` to
  `||`. An older copy also cut at or before k is never handed out as the
  replacement.
- `poll_serving` (~4410): guard `!same && !same_channel` to true, and `&&` to
  `||`. A group that closed short while the route came back to the same
  channel at a new generation must not reuse that channel's group. It may be
  equivalent given the `past_cut` check after it; if so, exclude it with the
  reason.

Likely equivalent, so prove and delete or exclude rather than test:

- `Recover::wants` (~4110, from [moq-dev/moq#9748](https://github.com/moq-dev/moq/pull/9748)):
  guard `Cause::Stalled if copy_failed` to false. The route-watch loop below
  returns true once the failed route moves, and both paths register on the
  route; deleting the arm is the likely result.
- `Reader::poll_first` (~3229, from [moq-dev/moq#9718](https://github.com/moq-dev/moq/pull/9718)):
  `is_unjudged(key) && window().holds(key)` to `||`. The guard only skips a
  `delivered.reopen(..)` that usually does nothing.

Older code, medium effort:

- `Consumer::register` (~656) to `()`: park a peek on a path that never
  looks at the route, change the route, and assert the waiter wakes. The work
  is finding such a path.

`just rs mutants` without `--in-place` copies the tree to `/tmp`, which mbx
cannot cache, so local reruns build cold; use `--in-place` in a scratch
worktree.

Public API: none. Wire: none.

## Related

- [Nightly passes on main](/quest/m0/nightly/README.md) - the epic whose triage found these
