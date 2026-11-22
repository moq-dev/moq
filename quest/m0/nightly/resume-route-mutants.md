# [M] No resume or relay route mutant survives

## Goal

Nightly's mutants shards report no survivors in
`rs/moq-net/src/model/resume.rs` or `rs/moq-net/src/route/relay.rs`.

## Plan

A local run on 2026-11-22 at
[moq-dev/moq#9326](https://github.com/moq-dev/moq/pull/9326)'s merge (`cargo mutants --in-diff`
over `resume.rs` and `route/` since Nightly's base 9df4313a, plus every mutant of the
functions below) found 23 survivors. The second PR on this quest killed or excluded 8, and
these 15 remain, all in `resume.rs`. Line numbers are from that PR's head; rerun the
functions on current main first, since they drift. Kill each with a unit test or a model
regression, or exclude it in `.cargo/mutants.toml` when it changes nothing observable.

The resume model (`model::resume_model`) is the cheapest first try: apply the mutant and
sweep a dozen seeds of 4096 cases (about 15 seconds each). None of these 15 fail a sweep of
seeds 1 to 6 and 9 to 14, so each needs what the model doesn't generate: live joins, a time
that differs from its group's key, or skips and refusals on a replaced copy.

- `Reader::resumed` (1830:9 guard `failed` to `false`, 1833:10 guard `current` to `true`
  and to `false`, 1837:5 and 1838:5 deleting `ranges` and `delay`): the resolution here
  mostly vanishes under `clamped`'s raise to the boundary, and `clamped` ignores it
  entirely while the reader has handed nothing out. A resolved start is never past the
  boundary, and an end past it does not resolve. Only a live join resolves to `None`
  rather than a raised start, which is the only case that can kill these. Under the test
  helpers, a group's start, its key, and its timestamp coincide, which hides any difference
  between a time and a key.
- `Reader::window` 1901:68 `<` to `>`, the cap filter on a resolved end: the serving copy
  usually resolves the same end, so a test needs a capped copy to be the only one that can
  place it.
- `Reader::resolve` 1935:26 `>` to `>=`, a time start of 0 waiting on a copy: a probe from
  time 0 under the resume point placed the range at 0 and read the same groups either
  way, because `backfill_gaps` places a raised start the reader holds at its own time.
  Probably equivalent.
- `Reader::drained` returning `true`: differs only when the route ended cleanly while the
  serving copy failed or no copy serves, where the track then waits for the replaced copies
  instead of ending.
- `Reader::poll_lowest` 2542:9 deleting `!` (a replaced copy's refusal fails the read):
  a replaced copy that refuses a moved time start had its refusal cleared before it was
  polled again in the one staged case (the serving copy's placement resends the ranges,
  which resets the refusal), and a copy that ran out is not polled at all. A test needs a
  replaced copy still draining whose refusal outlives the serving copy's placement.
- `Reader::poll_lowest` 2637:33 `&&` to `||` (a backfill copy of the serving route that
  reads late counts as serving): that copy reruns `backfill_gaps`, whose widening of a
  reused backfill resets that copy's `since`. A test would need that later budget to matter.
- `Reader::backfill` 3065:18 `|=` to `&=`: needs a b→c→b flap that widens a reused
  backfill with a raised start.
- `Reader::backfill_gaps` 3136:17 `<` to `<=` (probably equivalent, since `resolve` with an
  empty map differs from `window()` only while no copy has answered), 3162:19 guard to
  `false` and 3162:56 `>` to `<` (no reachable case found where the replaced copy's
  resolution of a start differs from the window's floor), 3210:71 `<` to `<=` (a range
  starting exactly at the resume point. It is delivered or drained either way, so this looks
  equivalent).

Whoever kills the `resolve` and `backfill_gaps` survivors also settles a possible
late-group ordering bug found while probing them, with a test, and fixes it if it is one.
Setup: a reader updates to `from(8)`, exactly at the replacement's resume point, while
the replacement is an empty received copy. Its window placed the range at start 0 rather
than floored, so a late group 7 went out after group 8 (which holds time 8) was already
delivered from the old route. It may match what a plain reader of that empty route does.

Public API: none. Wire: none.
