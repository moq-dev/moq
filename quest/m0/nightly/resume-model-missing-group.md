# [M] Resume model misses a group after give-ups and a route flap

## Goal

`MOQ_RESUME_SEED=2 MOQ_RESUME_CASES=4096` passes `model::resume_model::readers_match_the_logical_track`,
with the case it shrinks to kept as a regression, and the reader bug behind it fixed at
its root. Nightly sweeps a fresh seed, so a seed like this one fails it on some nights.

## Plan

Found on main at [moq-dev/moq#9531](https://github.com/moq-dev/moq/pull/9531) while
probing the [resume mutants](/quest/m0/nightly/resume-route-mutants.md). The shrunk case:

```text
GiveUp { instance: 0, slot: 0, group: 6 }
Write { instance: 0, slot: 0, group: None, frames: 1 }
GiveUp { instance: 0, slot: 0, group: 2 }
Join { reader: 0, spec: Spec { ordered: false, budget: Hour, range: None, drops: false } }
Up { instance: 0, slot: 1 }
Join { reader: 1, spec: Spec { ordered: false, budget: Hour, range: None, drops: true } }
Write { instance: 0, slot: 1, group: None, frames: 1 }
Down { instance: 0, slot: 1 }
finish: false
```

Reader 1 misses group 1: handed out `[0, 3, 4, 5, 7, 8, 9, 10, 11]`, dropped `{2, 6}`. Add
it to the model's `regressions()`, then reproduce it as a unit test in `resume.rs` and find
which copy should have delivered group 1. Decide first whether the model or the reader is
wrong: a reader joining after the give-ups, polling drops, on a route that comes up and
goes down.
