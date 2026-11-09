# [L] Nightly build and test cost history

## Goal

Every scheduled Nightly on `main` measures the gate's build and test cost
under fixed scenarios and publishes it to the benchmark history on
[git.moq.dev](https://git.moq.dev/bench/), broken down by module, Rust crate,
and test binary. A cost that moved shows up as a flagged series traceable to a
range of nightly commits, and the largest costs are visible to optimize.
Benchmarks are out of scope: Nightly's paired comparison, CodSpeed, and the
[performance audit](/quest/m3/audit-perf.md) already track them.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan):

- ✅ Build and test cost only (rejected: also extend benchmark tracking; also
  merge-queue latency). Benchmarks already have history, a paired gate, and a
  recurring audit; build and test time have none.
- ✅ Controlled nightly scenarios (rejected: harvest real CI runs; both;
  repoint moq-dev/rust2's `bench/build` harness at `main`). Queue runs are
  diff-scoped, cache-dependent, and share a loaded host, so their timings
  locate candidates but cannot attribute a change. rust2 pins one moq
  revision to compare toolchain levers, the opposite axis.
- ✅ Every `just ci` module (rejected: Rust only). Per module from dispatch,
  then per Rust crate and per test binary or test file where the module
  exposes it (cargo timings, nextest JUnit, bun).
- ✅ Two scenarios (rejected: cold gate only; rust2's full scenario set):
  a cold `just ci --all` with the Rust cache off, and a rebuild of the Rust
  tests after an edit to moq-net, the crate most diffs touch, as an agent's
  gate sees it.
- ✅ Publish into the existing costs history and table (rejected: a separate
  `ci/` Pages directory). A new source is a `bench-*` artifact with
  `report.json`, as [Benchmarks](/bench/README.md) describes; the
  `benchmark-site` job already validates, stores, and renders it.
- ✅ Flag, never fail (rejected: fail Nightly beyond a noise floor; post
  movers to a tracking issue). No fixed threshold. The table's trend flag
  marks movers, and the recurring audits triage them.
- ✅ The existing recurring audits read the history (rejected: a new
  recurring gate audit). This quest's PR points each one's "what to check
  first" at the series for its layer:
  [compile time](/quest/m3/audit-compile-time.md) per crate,
  [test cost](/quest/m3/audit-test-cost.md) per test binary, and the
  [CI workflow audit](/quest/m3/audit-ci.md) per module.

Recommendations, not settled:

- Run on a hosted x86 `ubuntu-24.04` runner with
  `kernel.perf_event_paranoid=2`, as rust2's `bench.yml` does, so builds
  report user-space instructions beside CPU and wall seconds. Instructions are
  the low-noise series for compiles; CPU seconds cover test runs, which are
  not instruction-bound. The report's unit list has no instruction count yet.
- Extend dispatch's per-module `report` with CPU time rather than adding a
  parallel instrument; it already records each phase's wall seconds.
- Cargo's `--timings` HTML carries per-unit data on stable. Prefer it over
  a nightly-only `-Z` flag.
- Record `/proc/loadavg` and the toolchain versions beside each night.

Public API: none. Wire: none.
