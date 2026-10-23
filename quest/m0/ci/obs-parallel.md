# [S] Bound and parallelize OBS source checks

## Goal

OBS syntax checks use available CPU concurrently after preparing their shared
headers, reducing queue latency without losing diagnostics or exceeding the
runner's resource budget. Failure and cancellation leave no child checks running.

## Plan

Decided 2026-10-23: parallelize independent translation units rather than
whole binding suites that share mutable Cargo outputs.

- `sh/obs/compile.sh` checks 20 source and test translation units sequentially.
  The complete OBS compile module took 46 seconds in
  [run 39996587979](https://github.com/moq-dev/moq/actions/runs/39996587979)
  and 92 seconds in
  [run 39993252909](https://github.com/moq-dev/moq/actions/runs/39993252909).
  These include header preparation and different workloads, so they identify
  a candidate rather than establish an expected parallel speedup.
- Finish `sh/obs/includes.sh` and its generated C++ package preparation before
  starting concurrent syntax checks. Preserve all source/test coverage,
  compiler flags, per-source diagnostics, and the aggregate failure result.
- Choose bounded concurrency from measurements under the effective CPU and
  memory limits, including competing work. Honor repository command admission
  when present; do not create a second scheduling policy or unbounded fanout.
- Verify a failing source makes the command fail, all selected sources are
  checked, and interruption reaps children. Compare serial and bounded
  parallel runs on the same inputs, reporting header preparation separately
  from compilation, wall time, peak memory, and impact on concurrent jobs.

## Related

- [Share local check capacity](/quest/m0/ci/local-gate.md) - coordinate internal fanout with the shared local command budget
