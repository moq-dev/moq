# [M] The failover and resume models mirror the code, and an ended track a reader holds keeps rerouting

## Goal

`FailoverChain.tla` and `ResumeReader.tla` describe the code that ships, their
skills' sync commits name a commit at or after the last covered change, and
an ended track that a reader still holds keeps moving a slow read to another
route after its 30 s linger.

## Plan

Found by the 2026-11-17 pre-cut correctness audit, from source.

- `.claude/skills/verify-failover/SKILL.md` still names 88572c583e and
  `verify-resume/SKILL.md` names 103052ed07, though
  [moq-dev/moq#8896](https://github.com/moq-dev/moq/pull/8896) edited both
  models and #8859, #8958, #8867, and #8638 changed covered code since.
  `ResumeReader.tla` cites `Route::finished`, which no longer exists. Run both
  skills on `main`, mirror what changed, and move the sync commits.
- The linger and `Action::Conclude` are not modelled, and `Mesh.cfg`, the only
  config that reaches the reroute, is sampled rather than exhaustive. Model the
  linger.
- `rs/moq-net/src/model/front.rs` and the `Forget` arm in `origin.rs`:
  `abort_unwanted` returns true for a closed track a reader still holds, so
  after the linger the track's I/O drops and a slow read of the last group
  fails when its upstream route dies. The `Reroute` path checks `is_held()`;
  `Forget` does not. Recommended: don't forget an ended track while it is
  held. Reproduce with the model before fixing. From #8896.

Public API: none. Wire: none.
