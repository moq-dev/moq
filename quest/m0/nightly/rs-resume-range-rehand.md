# [M] Rust resume hands a group out twice after its range is cleared

## Goal

`MOQ_RESUME_SEED=444444444 MOQ_RESUME_CASES=4096 just rs test -p moq-net --features async --lib -E 'test(=model::resume_model::readers_match_the_logical_track)'`
passes, and the shrunk case below is kept as a regression in both
`rs/moq-net/src/model/resume_model.rs` and `js/net/src/resume.model.test.ts`.

## Plan

Found while validating [moq-dev/moq#9748](https://github.com/moq-dev/moq/pull/9748): the
Rust half fails on `main` at 83e10f008a with
`reader 1: group 2 handed out twice: [2, 0, 4]`. The shrunk case:

```
write(0, 0, group 2, 1 frame); join(reader 1, unordered, hour, no range, no drops);
up(0, 2); up(0, 0); write(0, 0, lowest missing, 1 frame); write(0, 2, group 4, 1 frame);
ranges(reader 1, 4..5); ranges(reader 1, none)
```

The likely cause is `Record::retire`, which forgets what a retired range left behind, so the
range that covers it again hands it out a second time.
[moq-dev/moq#9718](https://github.com/moq-dev/moq/pull/9718) notes the same re-hand-out as
pre-existing, which contradicts `Subscriber::with_range`'s promise that a group is never handed
out twice. First check whether it still reproduces once moq-dev/moq#9718 lands.

Recommended: keep the promise by fixing the record. If that can't be done within the
record's memory bound, change the promise and both models' check together, in Rust and JS.

Public API: none. Wire: none.
