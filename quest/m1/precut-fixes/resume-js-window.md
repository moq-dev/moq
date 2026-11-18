# [M] A JS resumed reader hands out no group outside its window

## Goal

A `@moq/net` reader that resumes a track across routes (`js/net/src/resume.ts`)
never hands out a group outside the window its ranges resolved to, as the Rust
reader never does. `MOQ_RESUME_SEED=3 MOQ_RESUME_CASES=50000` and seeds 1 and 7
pass on `js/net/src/resume.model.test.ts`, so Nightly's `test/resume-model.sh`
passes on its JS side.

## Plan

Found by [moq-dev/moq#9103](https://github.com/moq-dev/moq/pull/9103) while
sweeping its fix: on `main` at [4f3ba3317f](https://github.com/moq-dev/moq/commit/4f3ba3317f),
the JS model fails fast with `group 0 outside ranges` (seed 3 at 89 cases,
seed 1 at 434, seed 7 at 196), with or without that PR.

```sh
MOQ_RESUME_SEED=3 MOQ_RESUME_CASES=50000 bun test ./js/net/src/resume.model.test.ts
```

Counterexample: join route 0 with range `[1, 3]`; route 0/1 comes up; write
group 1 (two frames), then group 0, then the next group on 0/0. The serving
copy resolves the time anchor against its own cache, which holds only group 0,
and hands group 0 out, though the reader's window starts at 1.

Introduced by [moq-dev/moq#9027](https://github.com/moq-dev/moq/pull/9027)
(`0c14c87c49`, "@moq/net addresses groups by start"): the same seeds pass on
its parent. Rust's `Reader::poll_lowest` filters each peeked group with
`self.window().contains(key)` ("the window decides"), leaving an ordered
cursor in place when `owes_below` still owes a lower group. JS `#lowest` and
the serving loop in `tryGroup` check only `#deliverable` (the copy's cap and
`delivered`). Mirror that filter, with the window built from `#placed` /
`resolved()`, and add the counterexample to `resume.model.test.ts`'s
regressions.

Public API: none. Wire: none.
