# [M] Dead code and API surface audit

## Goal

Code, exports, Cargo features, and CLI flags that nothing uses are deleted, or
are planned for deletion when removal breaks a public API.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.


Removing a public item is a breaking change and the maintainer's call, so it is
always a follow-up with its `MIGRATE.md` entry, never an inline fix.

The m0 run (2026-10-19) removed seven unused JS wire codec classes, internal
aliases and token constants, module-local exports, two unused SVGs, one unused
formatter, and two unused build dependencies. Rust's draft-17 PUBLISH_BLOCKED
codec now compiles only for tests/fuzzing; six stale lite lint suppressions
were removed. An all-features-only unused moq-stats benchmark import was
also removed. Public entrypoints, Cargo features, flags, and wire behavior
were preserved.

Start by comparing the live tree against these baselines:

- Rust had 410 dead-code/unused suppression attributes, 331 conditional on
  features or platforms; 403 remain after this run. Keep the conditional
  model/session separation, generated settings and NVENC ABI tables, retained
  drop guards, compile-time Send assertions, deserialization rejection
  fixtures, and shared test/benchmark harnesses. Recheck their actual callers
  rather than deleting attributes by count. `cargo shear` found no unused
  dependencies. The wasm-only getrandom dependency selects a backend.
- Knip 6.42.0 found the JS removals above; the scoped final run had no unused
  JS exports, types, or files. Its defaults miss Bun tests, TSX,
  worklets, shell-launched test clients, and documentation imports, and treat
  MPEG-TS fixtures as TypeScript. Supply those entrypoints explicitly, inspect
  package `exports`, and search the whole tree before removing a finding.
  Preserve the TypeScript DOM override dependency; it is compiler-selected.
  Preserve `rolldown` in every package whose Vite config loads
  `js/common/vite-plugin-worklet.ts`; `js/common` has no manifest of its own.
  Keep the one-off analyzer outside the package dependency graph unless its
  false positives can be reduced enough for CI.
- There were 144 Cargo feature declarations across 25 manifests. The
  moq-native tombstone's empty compatibility features, the internal transport
  and test marker features, platform gates, and all-features nightly coverage
  are deliberate. GCS/Azure select object_store backends and the QUIC
  crypto/logging features serve external consumers; lack of an explicit
  downstream feature spelling does not make them dead.
- The usage registries declared 118 distinct long flags. Registry drift tests
  cover the generated settings surface. Trace a setting through the config
  merge and runtime consumer; a flag missing from prose/examples alone is
  not evidence that it is unused. Preserve hidden deprecated spellings that
  fail with migration guidance.

No public removal was justified by this run. Propose any future public
removals separately with their migration and consumer evidence. The public
model and performance audits own their API and benchmark decisions.

A bounded follow-up on 2026-10-20 checked the source changes between the m0
audit merge (`e900b9f62`) and `49c78f9b0`: only the TS carriage diagnostic
helper and its tests changed. The helper has two callers; no JS, dependency,
feature, flag, or unused-code suppression was added. Cargo-shear 1.14.0 found
one redundant `moq-uring` self-dependency ignore, removed after confirming
the crate's integration tests and benchmarks import it. Its post-removal run
had no findings. This is incremental evidence, not completion of the m1
release-cycle audit; the baselines above still need their next full pass.
