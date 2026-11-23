# [M] The resume model sweep fails a group as stale under an hour budget

## Goal

`test/resume-model.sh` (`MOQ_RESUME_CASES=4096`) passes on `main` for any
seed, in Rust and JS, fixed at its cause.

## Plan

Since [moq-dev/moq#9409](https://github.com/moq-dev/moq/pull/9409) (Max Age
is a Live range's staleness budget), both sweeps fail within the first few
hundred cases of every seed tried (1 to 8). Seed 1 passes on its parent,
e9b1a5ff48. PRs run a fixed 64 cases, which miss it, so only Nightly's
`test resume-model` job reports it.

With one route and no switch, a reader with an hour budget and no ranges
reads 0 to 3, then narrows its range to one group, and a group it was
reading stops part-way as stale:

- Rust, `MOQ_RESUME_SEED=1`: `reader 1: group 2 stopped at frame 1
  (Some(Err(Old)))`, ops `Join(reader 1, hour)`, four writes (the last
  group 3), `Ranges(reader 1, 3..4)`.
- JS, `MOQ_RESUME_SEED=1`: `reader 3: group 1 stopped at frame 1 (group
  exceeded the subscription max delay budget)`, ops three writes from group
  2, `join(reader 3, hour)`, `ranges(reader 3, [2, 3])`, `up`.

Decide first whether the implementation or the model is wrong: after
#9409, does narrowing a reader's range change the budget a group it already
holds is judged by? If the group is still owed, fix the reader; if the new
rule makes it stale, teach the model the rule.
