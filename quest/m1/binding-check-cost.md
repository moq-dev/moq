# [M] Binding checks cost less

## Goal

An `rs/moq-ffi` change's binding checks finish markedly faster, with the
same coverage. `moq.cpp` compiles once per C++ standard instead of once per
consumer build, and Kotlin reuses a warm Gradle daemon. The PR reports the
before and after.

## Plan

From the CI audit in [moq-dev/moq#6155](https://github.com/moq-dev/moq/pull/6155),
decided 2026-10-21 with the maintainer as an m1 quest. Measured there: C++
plus the OBS plugin take about 4.5 min because the generated `moq.cpp`
compiles six times at `-O0`, and Kotlin spends about 65 s of its 30 to 110 s
starting a cold Gradle daemon. Those two are most of the binding time.

- Build `moq.cpp` into one library per C++ standard and link every consumer
  against it.
- Keep a Gradle daemon warm where the checks run, without letting it leak
  state between builds.
- Measure locally and on the queue runner, before and after.

Public API: none. Wire: none.

## Related

- [CI workflow audit](/quest/m1/audit-ci.md) - judges every job's cost against its signal
- [CI cost and latency](/quest/m0/ci/README.md) - runner and cache infrastructure
