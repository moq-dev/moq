# [S] The JS resume model finds no missing groups

## Goal

Nightly's `Tests (test resume-model)` passes: the JS resume model property
test (`js/net/src/resume.model.test.ts`) hands every reader each group its
spec owes on every generated case. Find the root cause and fix it in
`@moq/net`, or in the model if the model is wrong.

## Plan

Added 2026-11-05 (maintainer, /quest-audit), from scheduled Nightly
[run 42050683248](https://github.com/moq-dev/moq/actions/runs/42050683248/job/127325640208).
The Rust half (`model::resume_model::readers_match_the_logical_track`)
passed; the JS half failed:

```text
error: Property failed after 2700 tests
error: reader 0: missing groups 2,3,4,5,6,7,8,9,10; handed out 1,11,0, dropped , floor undefined, spec {"ordered":false,"budget":"hour","drops":false}
```

Replay it with `MOQ_RESUME_SEED=764723972 just test resume-model`. The shrunk
counterexample: instance 1 brings up slots 1 and 2, reader 0 joins unordered
with an hour budget, slot 1 writes group 11 and slot 2 group 1, slot 2 goes
down, slot 1 ends, and reader 0 joins again.

The two earlier scheduled runs failed this job with a different symptom,
`the track ended cleanly though no route declared an end`
([41852517544](https://github.com/moq-dev/moq/actions/runs/41852517544/job/126652296304),
[41656613559](https://github.com/moq-dev/moq/actions/runs/41656613559/job/125988510774)).
Check whether one cause explains both before closing this.

Public API: none expected. Wire: none.
