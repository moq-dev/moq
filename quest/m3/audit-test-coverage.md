# [M] Test coverage audit

## Goal

Behavior that matters but no test exercises is covered or planned, and every
test runs in CI.

## Plan

Recurs each major release: the PR that finishes a run keeps its results in
its own body and re-adds this file at the end of m3's `Required`, with its own `Required`
naming the [release cut](/quest/m3/release/cut.md) and Check first rewritten. Fixes land
in that PR; only maintainer decisions (public API, scope) and changes too
large to review alongside become follow-ups.

Test cost belongs to the [cost audit](/quest/m3/audit-cost.md).

Previous run, with its per-crate and per-package baselines:
[moq-dev/moq#6175](https://github.com/moq-dev/moq/pull/6175).

### Check first

- Start from Nightly's `rust-coverage` and `js-coverage` artifacts
  (`just rs coverage`, `just js coverage`) rather than re-running the
  instrumented workspace. `just rs coverage <crate>` is not comparable to the
  workspace report.
- The nightly legs that run tests outside `just check`, such as `rs features`
  and `rs audio-devices`: a red leg hides everything after it, so confirm each
  passed before calling a file untested.
- Public functions never executed: walk the lcov `DA:` records for
  `pub fn`/`pub(crate) fn` lines outside tests, benches, and fuzz, starting
  with moq-c, moq-ffi, and moq-mux's import facades. Per-file uncovered counts
  mostly point at GPU code and generated bindings.
- Bun undercounts a module loaded again with a query suffix; verify a JS gap
  before acting, and never merge packages into one `bun test` process.
- `just gh check` already fails on an `#[ignore]` test or test target no
  workflow reaches; the local-only ones are listed in
  `sh/gh/test-inventory.allow` with their owning quests.
- Sample `fix` commits for regression tests, counting `#[kio::test]`.
- Whether [interop bindings](/quest/m3/interop-bindings.md) and
  [GPU CI](/quest/m3/gpu-ci.md) landed, before re-auditing the tests they own.
- Each TLA+ model's `verify-*` skill against the code it covers: run
  `/maintain-formal-verification-skill` on every one whose Coverage files
  changed since its sync commit, so a change that skipped review is resynced
  (decided 2026-11-10).

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
