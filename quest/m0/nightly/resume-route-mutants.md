# [M] No resume or relay route mutant survives

## Goal

Nightly's mutants shards report no survivors in
`rs/moq-net/src/model/resume.rs` or `rs/moq-net/src/route/relay.rs`.

## Plan

A local run on 2026-11-21 (`cargo mutants --in-diff` over `resume.rs` and `route/`
since Nightly's base 9df4313a, plus `--re 'in (Resume::add|Reader::resumed)$'`) found 34
survivors, most from [moq-dev/moq#9278](https://github.com/moq-dev/moq/pull/9278) and
[moq-dev/moq#9307](https://github.com/moq-dev/moq/pull/9307). The first PR on this quest
killed or excluded 18. Line numbers are from that run; rerun
`cargo mutants -f rs/moq-net/src/model/resume.rs` on current main first, since they drift.
Kill each with a unit test, or exclude it in `.cargo/mutants.toml` when it changes nothing
observable.

These 16 remain, with what is known about each:

- `Reader::resumed` (1792:16 `&&` to `||`, 1796:9 guard `failed` to `false`, 1799:10
  guard `current` to `true` and to `false`, 1803:5 and 1804:5 deleting `ranges` and
  `delay`): the resolution here mostly vanishes under `clamped`'s raise to the boundary.
  A resolved start is never past the boundary, and an end past it does not resolve. Only
  a live join resolves to `None` rather than a raised start, which is the only case that can
  kill these. Under the test helpers, a group's start, its key, and its timestamp coincide,
  which hides any difference between a time and a key.
- `Reader::window` 1867:68 `<` to `>`, the cap filter on a resolved end: the serving copy
  usually resolves the same end, so a test needs a capped copy to be the only one that can
  place it.
- `Reader::resolve` 1901:26 `>` to `>=`, a time start of 0 waiting on a copy: a probe from
  time 0 under the resume point placed the range at 0 and read the same groups either
  way, because `backfill_gaps` places a raised start the reader holds at its own time.
  Probably equivalent.
- `Reader::drained` returning `true`, and 2303:21 `&&` to `||` (resetting `since` on
  replaced copies a group already superseded): the reader's own delivery budget gives up
  the same gaps at the same time, so a test needs a gap younger than the copy's `since`.
  Returning `true` differs only when the route ended cleanly while the serving copy failed
  or no copy serves, where the track then waits for the replaced copies instead of ending.
- `Reader::poll_lowest` 2544:33 `&&` to `||` (a backfill copy of the serving route that
  reads late counts as serving): that copy reruns `backfill_gaps`, whose widening of a
  reused backfill resets that copy's `since`. A test would need that later budget to matter.
- `Reader::backfill` 2928:18 `|=` to `&=`: needs a b→c→b flap that widens a reused
  backfill with a raised start.
- `Reader::backfill_gaps` 2999:17 `<` to `<=` (probably equivalent, since `resolve` with an
  empty map differs from `window()` only while no copy has answered), 3025:19 guard to
  `false` and 3025:56 `>` to `<` (no reachable case found where the replaced copy's
  resolution of a start differs from the window's floor), 3073:71 `<` to `<=` (a range
  starting exactly at the resume point. It is delivered or drained either way, so this looks
  equivalent).

Whoever kills the `resolve` and `backfill_gaps` survivors also settles a possible
late-group ordering bug found while probing them, with a test, and fixes it if it is one.
Setup: a reader updates to `from(8)`, exactly at the replacement's resume point, while
the replacement is an empty received copy. Its window placed the range at start 0 rather
than floored, so a late group 7 went out after group 8 (which holds time 8) was already
delivered from the old route. It may match what a plain reader of that empty route does.

Public API: none. Wire: none.

## Required

- [Resumed read ends once its window is accounted for](/quest/m0/nightly/resume-exhausted-skips.md) - its rework of `model/resume.rs` must land first, since these survivors' lines and behavior move with it
