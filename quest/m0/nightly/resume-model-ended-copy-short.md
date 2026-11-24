# [M] The JS resume model ends a group a copy closed short after declaring the end

## Goal

`test/resume-model.sh` passes on `main` for any seed in JS, with the JS
front matching the Rust one, fixed at its cause.

## Plan

Found by `MOQ_RESUME_SEED=472819650` (JS only, about 1 seed in 13), once
the stale-budget failure that hid it was fixed:

```text
reader 0: group 11 ended after 1 frames where a failed route closed it
```

It replays deterministically as a case in `js/net/src/resume.model.test.ts`
`regressions`, and the same ops pass in the Rust model:

```ts
{ kind: "join", reader: 0, spec: spec(false, "hour", undefined, false) },
{ kind: "up", instance: 0, slot: 2 },
{ kind: "write", instance: 0, slot: 0, group: 11, frames: 1 },
{ kind: "end", instance: 0, slot: 2 },
{ kind: "close", instance: 0, slot: 2, group: undefined, down: true },
```

The cheaper slot 2 takes over and fills group 11, the last group, at one
frame. It declares the track's end, closes group 11 short there, and fails.
Slot 0 still writes group 11, so the reader should read on from it. Instead
JS ends the group at the failed copy's close. Dropping the `end` op, or using
any group but the last, passes. So the likely cause is how the JS front
(`js/net/src/resume.ts`, `#end` and `#learnEnd`) treats a failed copy's
declared end as settling a group it closed short. Compare it with the Rust
front's handling in `rs/moq-net/src/model/resume.rs`.
