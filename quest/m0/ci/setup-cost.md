# [S] Remove unnecessary CI setup work

## Goal

Selected CI jobs avoid disk cleanup and setup work their workload does not
need, reducing latency and CPU without moving the existing Check/Test gates.

## Plan

- An October 2026 sample showed hosted-runner disk cleanup taking roughly
  43-56 seconds in Check/Test jobs. Re-measure current workflows before
  changing them: [run 38835909360](https://github.com/moq-dev/moq/actions/runs/38835909360)
  and [run 38835480519](https://github.com/moq-dev/moq/actions/runs/38835480519).
- Use the existing scope selection before expensive setup. If cleanup is
  conditional, derive required free-space margin from actual selected build
  peaks and retain a clear failure when space is insufficient.
- Audit repeated installs and builds only where equivalent outputs can be
  reused. Record before/after setup and total job timings, including cache
  misses; avoid trading lower reported setup for hidden repeated work.
- Decided 2026-10-15: m0. Keep hosted runners, PR Check/Test, and the platform
  merge-queue policy. Existing R2/Nix cache and FFI-reuse quests keep ownership.
