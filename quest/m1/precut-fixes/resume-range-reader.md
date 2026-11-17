# [M] A ranged reader resumed across routes gets exactly the groups in its range

## Goal

A reader that asks a resumed track (`rs/moq-net/src/model/resume.rs`) for a
range of groups is handed every group of the logical track in that range, and
none outside it, whichever routes come up, write, and end while it reads.
The resume model's random sweep (`model/resume_model.rs`) passes on that
seed, and the resume TLA+ model (`/verify-resume`) checks the fix.

## Plan

Found by [moq-dev/moq#9019](https://github.com/moq-dev/moq/pull/9019) while
killing `Reader::window` mutants: the resume model fails on unmutated `main`.
Reproduced on `main` at [2dcb5f8994](https://github.com/moq-dev/moq/commit/2dcb5f8994):

```sh
MOQ_RESUME_SEED=3 MOQ_RESUME_CASES=50000 cargo test -p moq-net --all-features --lib readers_match_the_logical_track
```

> reader 0: missing groups [6, 7]; handed out [0, 2, 1], dropped {}, floor None,
> spec Spec { ordered: false, budget: Hour, range: Some((6, 8)), drops: false }

Shrunk ops: `Join r1 live; Up 0/1; Join r0 (6,8); Write 0/0 next; Write 0/1 g11; Write 0/1 g2; End 0/1`.

Decided 2026-11-17 (maintainer, [moq-dev/moq#9019](https://github.com/moq-dev/moq/pull/9019#issuecomment-6961416403)):
✅ a quest in this epic, sized M, so it gates the `0.22.0` cut (rejected: a
standalone m1 quest outside the gate; dropping it).

- Reproduce with that seed, then pin the shrunk case as a unit test in
  `resume_model.rs` next to `unchanged_routes_keep_groups_owed`.
- Fix at the root in `resume.rs`, wherever the reader's range stops bounding
  what it picks and what it waits for (`Reader::window`, `poll_lowest`,
  `deliverable`, `poll_end` are the likely places). No widening of the model's
  expectations.
- Check the fix with `/verify-resume` (`rs/moq-net/verify/resume/`), extending
  `ResumeReader.tla` with a ranged reader if it has none, so every ordering is
  checked rather than one seed.
- Settles the two `Reader::window` mutants [moq-dev/moq#9019](https://github.com/moq-dev/moq/pull/9019) left unexcluded (`> → >=` on
  the capped-copy filter and `< → >` on the cap on the end): once the range
  logic is fixed, kill each with a test or show it equivalent and exclude it
  in `.cargo/mutants.toml` with the reason.
- Find the culprit for `## Caused by` by blaming the fixed lines.
