# [M] No mutant of the resume reader's window survives its tests

## Goal

Nightly's mutants job never flags `rs/moq-net/src/model/resume.rs`
`Reader::window`: each of its two surviving mutants is killed by a test, or
shown equivalent and excluded in `.cargo/mutants.toml` with the reason.

## Plan

[moq-dev/moq#9019](https://github.com/moq-dev/moq/pull/9019) left both
unexcluded, expecting the resume model's seed-3 failure to be range logic in
`Reader::window`. It was not: [moq-dev/moq#9103](https://github.com/moq-dev/moq/pull/9103)
found a cleanly drained copy dropping groups that landed late, and its fix
leaves `Reader::window` as it was. The mutants:

- `> → >=` in the capped-copy filter (`resolved.end.is_none_or(|end| end > resolved.start)`):
  differs only when a capped copy resolves a range to an empty one ending at
  its start, which the original treats as saying nothing and the mutant
  places as an empty window.
- `< → >` on the cap on the end (`cap.is_none_or(|cap| *end < cap)`): a capped
  copy's own wire ends at its cap, so its resolved end never passes it, and the
  mutant drops every end the original keeps. The window then waits for another
  copy to place its end. This shows only when a ranges change leaves the
  window's time end unsettled on the serving copy while a replaced, capped copy
  already holds a group past it. A bounded read then ends at its range's end
  on the original, and stays open on the mutant until the track ends.
  `resume_model.rs` accepts both, an open read and one ended at its range,
  which is why 60k sweep cases never killed it: a unit test in `resume.rs`'s
  `mod test` that asserts the read ends is the likely kill.

Public API: none. Wire: none.

## Related

- [Mutants of the other modelled code](/quest/m4/mutants-modelled.md) - the same Nightly job, widened to more files
