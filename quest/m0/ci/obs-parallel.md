# [S] Parallelize OBS source checks

## Goal

When OBS's syntax checks are on a queue entry's critical path after modules run
in parallel, they check independent translation units concurrently after
preparing their shared headers, without losing diagnostics or exceeding the
fanout bound modules share. Failure and cancellation leave no child checks
running.

## Plan

Decided 2026-10-23: module-level parallelism lands first, and this adds
translation-unit fanout inside `obs_compile` only where it still shortens the
entry.

- `sh/obs/compile.sh` checks 20 source and test translation units sequentially.
  The complete OBS compile module took 46 seconds in
  [run 39996587979](https://github.com/moq-dev/moq/actions/runs/39996587979)
  and 92 seconds in
  [run 39993252909](https://github.com/moq-dev/moq/actions/runs/39993252909),
  including header preparation, which these samples do not separate.
- Once modules overlap, measure whether `obs_compile` ends an entry, such as
  one touching only `cpp/obs/`. If it never does, delete this quest.
- Finish `sh/obs/includes.sh` and its generated C++ package preparation before
  starting concurrent syntax checks. Preserve all source/test coverage,
  compiler flags, per-source diagnostics, and the aggregate failure result.
- Take concurrency from the bound parallel modules and local admission share;
  do not create a second scheduling policy or unbounded fanout.
- Verify a failing source makes the command fail, all sources are checked, and
  interruption reaps children. Compare serial and parallel runs on the same
  inputs, reporting header preparation separately from compilation.

## Required

- [Run the queue's modules in parallel](/quest/m0/ci/parallel-modules.md) - decides whether OBS stays on the critical path, and owns the fanout bound

## Related

- [Share local check capacity](/quest/m0/ci/local-gate.md) - coordinate internal fanout with the shared local command budget
