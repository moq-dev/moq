# [M] No resume or relay route mutant survives

## Goal

Nightly's mutants shards report no survivors in
`rs/moq-net/src/model/resume.rs` or `rs/moq-net/src/route/relay.rs`.

## Plan

Two local runs found the survivors below; kill each with a unit test or a model regression,
or exclude it in `.cargo/mutants.toml` when it changes nothing observable. Line numbers are
from [moq-dev/moq#9393](https://github.com/moq-dev/moq/pull/9393)'s merge of main at
[moq-dev/moq#9490](https://github.com/moq-dev/moq/pull/9490); rerun the functions on
current main first, since they drift. The resume model (`model::resume_model`) is the
cheapest first try: apply the mutant and sweep a dozen seeds of 4096 cases (about 15
seconds each).

### Left from the 2026-11-22 run

A run at [moq-dev/moq#9326](https://github.com/moq-dev/moq/pull/9326)'s merge found 23;
the second PR on this quest killed or excluded 8, and one more (`Reader::resumed`'s
`current` guard to `false`) is caught on current main. None of these 14 fails a sweep of
seeds 1 to 6 and 9 to 14, so each needs what the model doesn't generate: live joins, a
time that differs from its group's key, or skips and refusals on a replaced copy.

- `Reader::resumed` (2061:9 guard `failed` to `false`, 2064:10 guard `current` to `true`,
  2070:6 and 2071:6 deleting `ranges` and `placed`): the resolution here mostly vanishes
  under `clamped`'s raise to the boundary, and `clamped` ignores it entirely while the
  reader has handed nothing out. A resolved start is never past the boundary, and an end
  past it does not resolve. Only a live join resolves to `None` rather than a raised start,
  which is the only case that can kill these. Under the test helpers, a group's start, its
  key, and its timestamp coincide, which hides any difference between a time and a key.
- `Reader::window` 2143:68 `<` to `>`, the cap filter on a resolved end: the serving copy
  usually resolves the same end, so a test needs a capped copy to be the only one that can
  place it.
- `Reader::resolve` 2176:40 `>` to `>=`, a time start of 0 waiting on a copy: a probe from
  time 0 under the resume point placed the range at 0 and read the same groups either
  way, because `backfill_gaps` places a raised start the reader holds at its own time.
  Probably equivalent.
- `Reader::drained` returning `true`: differs only when the route ended cleanly while the
  serving copy failed or no copy serves, where the track then waits for the replaced copies
  instead of ending.
- `Reader::poll_lowest` 2874:9 deleting `!` (a replaced copy's refusal fails the read):
  a replaced copy that refuses a moved time start had its refusal cleared before it was
  polled again in the one staged case (the serving copy's placement resends the ranges,
  which resets the refusal), and a copy that ran out is not polled at all. A test needs a
  replaced copy still draining whose refusal outlives the serving copy's placement.
- `Reader::poll_lowest` 2969:33 `&&` to `||` (a backfill copy of the serving route that
  reads late counts as serving): that copy reruns `backfill_gaps`, whose widening of a
  reused backfill resets that copy's `since`. A test would need that later budget to matter.
- `Reader::backfill` 3430:18 `|=` to `&=`: needs a b→c→b flap that widens a reused
  backfill with a raised start.
- `Reader::backfill_gaps` 3508:17 `<` to `<=` (probably equivalent, since `resolve` with an
  empty map differs from `window()` only while no copy has answered), 3534:33 guard to
  `false` and 3534:70 `>` to `<` (no reachable case found where the replaced copy's
  resolution of a start differs from the window's floor), 3591:72 `<` to `<=` (a range
  starting exactly at the resume point. It is delivered or drained either way, so this looks
  equivalent).

### New in Nightly's 2026-11-23 run

Nightly run [45242597151](https://github.com/moq-dev/moq/actions/runs/45242597151) (at
[moq-dev/moq#9409](https://github.com/moq-dev/moq/pull/9409)) missed 27 in code changed
since the night before. Rerun on the merge above, 19 still survive besides the
`Reader::resumed` and `Reader::resolve` ones listed above; `Reader::ask`'s
`floor <= keys.start` and `end >= keys.end` mutants are caught since
[moq-dev/moq#9468](https://github.com/moq-dev/moq/pull/9468), and `Reader::sync`'s
unchanged-ranges guard to `false` is excluded with the same reason as before. Not yet
probed:

- `Copy::cap`, from [moq-dev/moq#9402](https://github.com/moq-dev/moq/pull/9402): every
  mutant of `narrower` (945:52 `&&` to `||`; 950:11 `<` to `==` and `<=`; 950:20 `||` to
  `&&`; 950:29 `==` to `!=`; 950:39 `&&` to `||`; 950:97 `<` to `==`, `>`, and `<=`).
  Nothing caps an already-capped copy again, so a test needs a second switch that moves a
  replaced copy's cap, narrower and wider, within one group and across groups.
- `Copy::asked_for` 811:39 `<` to `<=`, `Reader::asking` 2689:44 `&&` to `||` and 2705:14
  `<` to `<=`, and `Reader::ask` 3119:5 `&&` to `||` (an asked copy must share the serving
  copy's channel), from [moq-dev/moq#9435](https://github.com/moq-dev/moq/pull/9435).
- `Reader::owed` 2633:27 `<=` to `>` and 2637:54 deleting `!` (a fill copy also counts as
  feeding), from [moq-dev/moq#9407](https://github.com/moq-dev/moq/pull/9407).
- `Reader::backfill_gaps` 3568:24 guard `copy.replaced` to `true`.
- `Reader::sync` 2311:7 deleting `joining` and 2442:17 `==` to `!=`, from
  [moq-dev/moq#9316](https://github.com/moq-dev/moq/pull/9316) and
  [moq-dev/moq#9402](https://github.com/moq-dev/moq/pull/9402).
- `Router::disconnect` in `route/relay.rs` 1225:30 `<` to `<=` (an older session to the same
  relay), from [moq-dev/moq#9370](https://github.com/moq-dev/moq/pull/9370).

The code changed after Nightly's base (60 mutants in `resume.rs` and `route/`, from
[moq-dev/moq#9444](https://github.com/moq-dev/moq/pull/9444) onward) was not run here;
the next Nightly's changed shards cover it.

Whoever kills the `resolve` and `backfill_gaps` survivors also settles a possible
late-group ordering bug found while probing them, with a test, and fixes it if it is one.
Setup: a reader updates to `from(8)`, exactly at the replacement's resume point, while
the replacement is an empty received copy. Its window placed the range at start 0 rather
than floored, so a late group 7 went out after group 8 (which holds time 8) was already
delivered from the old route. It may match what a plain reader of that empty route does.

Public API: none. Wire: none.
