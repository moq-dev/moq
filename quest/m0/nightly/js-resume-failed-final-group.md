# [M] JS resume reads a failed route's final group on from its replacement

## Goal

`MOQ_RESUME_SEED=309058592 MOQ_RESUME_CASES=4096 bun test ./js/net/src/resume.model.test.ts`
passes, and the JS resume model agrees with Rust on the case below, kept as a regression in
both `js/net/src/resume.model.test.ts` and `rs/moq-net/src/model/resume_model.rs`.

## Plan

Found by the Nightly triage of 2026-11-26: `Tests (test resume-model)` in run
[45667362304](https://github.com/moq-dev/moq/actions/runs/45667362304) failed the JS half
with `reader 0: group 11 ended after 1 frames where a failed route closed it`, while the
Rust half passed the same seed. The shrunk case:

```
up(0, 1); write(0, 1, group 11, 1 frame); end(0, 1);
join(reader 0, unordered, hour, no range, no drops);
close(0, 1, open group, down); write(0, 0, group 0, 1 frame)
```

The route at slot 1 finishes the track at its final group (11) while that group is still
open, then fails with the group closed short. Rust keeps the reader's group 11 open for a
replacing route; JS ends it short. The same ops added to Rust's `regressions` pass on `main`,
so the bug is in `js/net`, not the model.

It fails on every `js/net/src` revision checked back to
[moq-dev/moq#9467](https://github.com/moq-dev/moq/pull/9467) and well before, so no single
recent PR caused it; random seeds rarely reach it.

Find where the JS front treats a finished track's short final group from a failed route as
ended (likely the finish taking precedence over the failover), match Rust, and keep the case
as a regression on both sides.

Public API: none. Wire: none.
