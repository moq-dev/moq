# [M] resume_model panics on unordered reader narrowing ranges after GiveUp

## Goal

`model::resume_model::readers_match_the_logical_track` passes on seed `1465722607` (and Nightly's `Tests (test resume-model)` passes), with a unit test pinning the root cause.

## Plan

Found in Nightly [45242597151](https://github.com/moq-dev/moq/actions/runs/45242597151) (2026-11-23, `b29b90be29`, job `137930913206`):

```text
MOQ_RESUME_SEED=1465722607 MOQ_RESUME_CASES=4096
thread 'model::resume_model::readers_match_the_logical_track' panicked at rs/moq-net/src/model/resume_model.rs:1158:44:
called `Result::unwrap()` on an `Err` value: Fail(
  Reason("reader 2: group 2 stopped at frame 1 (Some(Err(Old))); handed out [0, 3, 2], dropped {}, floor Some(0), spec Spec { ordered: false, budget: Hour, range: None, drops: false }"),
  Case { ops: [
    Write { instance: 0, slot: 0, group: None, frames: 1 },
    Join { reader: 2, spec: Spec { ordered: false, budget: Hour, range: None, drops: false } },
    GiveUp { instance: 0, slot: 0, group: 1 },
    Write { instance: 0, slot: 0, group: Some(3), frames: 1 },
    Write { instance: 0, slot: 0, group: None, frames: 1 },
    Ranges { reader: 2, range: Some((3, 4)) }
  ], finish: false }
)
```

Reproduce with `MOQ_RESUME_SEED=1465722607 MOQ_RESUME_CASES=4096 just test resume-model`, diagnose why narrowing reader 2's range to `(3, 4)` stopped an already-handed-out group 2 with `Err(Old)` (or why the model expected it to finish), fix at the root cause, and check whether [verify-resume](/.agents/skills/verify-resume/SKILL.md) should mirror the scenario.

Public API: none. Wire: none.
