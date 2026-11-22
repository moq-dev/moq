# [M] Cut cross-language just test CPU cost

## Goal

Cut `just test` (`test/justfile`) CPU consumption (~20 CPU-min average, ~55.4
CPU-h/week across 167 runs on the build host) by reusing already-built workspace
artifacts and eliminating redundant rebuilds across harness and interop runs.

## Plan

Host telemetry over 7 days shows `just test` running 167 times for 55.4
CPU-hours (19.9 CPU-min mean, 16.9 CPU-min median, 30.3 CPU-min p90, 362s
average wall).

- Break down `just test` CPU cost across its sub-recipes (`harness`, `interop`,
  `drill`, `audio-quality`) to locate which compilations and test runs dominate
  the 55 CPU-h/week total.
- Reuse already-built workspace binaries (`moq-relay`, `moq-cli`, `moq-ffi`)
  and avoid redundant Cargo or binding rebuilds when running cross-language
  tests repeatedly during local iteration.
- Parallelize independent unit/script checks inside `test/justfile` `harness`
  while keeping Chromium/PulseAudio browser tests isolated from interference.
- Measure `just test` CPU-minutes and wall time before and after.

Decided 2026-11-21 (afk, `/quest-plan`):

- Placement: ✅ `quest/m0/ci/interop-test-cpu.md` (belongs in the active m0 CI
  cost and latency epic; rejected: top-level `m0`, `m3`).
- Approach: ✅ reuse compiled artifacts and parallelize independent harness
  checks without dropping interop matrix coverage (rejected: shrinking the
  interop publisher/subscriber matrix).
