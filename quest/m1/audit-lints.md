# [M] Lint override and deprecation audit

## Goal

Every lint suppression in first-party code, in every language, is still
needed and says why, or is removed. Nothing calls a deprecated upstream API,
and our own deprecated items are removed or planned for removal once their
migration window has passed.

## Plan

Recurs once per major release (decided 2026-10-20). The PR that finishes
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

Scope is every language (decided 2026-10-20): clippy and rustc overrides,
`clippy.toml`, `biome-ignore`, `@ts-expect-error`, `noqa` and
`type: ignore`, Kotlin `@Suppress`, and the equivalents in Go, Swift, Dart,
and C/C++. `dead_code` and `unused` suppressions belong to the [dead code
audit](/quest/m2/audit-dead-code.md); skip them here. Generated or vendored
bindings (`moq-nvenc`, `moq-v4l/src/sys`, `moq-rtmp/src/rml`, and the
untracked UniFFI output) keep their blanket allows unless the generator can
emit clean code. Shell `shellcheck disable` comments stay out of scope.

A suppression stays only when fixing the lint would make the code worse; a
signature reshaped to satisfy `too_many_arguments` or `large_enum_variant`
on a public type is an API change, so it is a follow-up. Removing one of our
own deprecated exports or hidden CLI spellings is a breaking change with a
`MIGRATE.md` entry, so it is also a follow-up.

### Remaining work

This run remains open until the Apple-native checks can run. Do not add the
m2 successor before finishing these checks and the audit's fixes.

- Run Swift build/tests on macOS and inspect SDK deprecation warnings.
  `just swift check` skipped on the Linux audit host because Swift was absent;
  searching and reading the thin Swift wrappers found no explicit deprecated
  declarations, but does not establish that their SDK calls are warning-free.
- Run macOS clippy for `moq-sock` and `moq-video`, including capture.
  The Apple-only `msghdr_x` naming expectation was stale and is removed:
  Rust exempts `repr(C)` structs from `non_camel_case_types`. A minimal example
  under the pinned compiler reproduced an unfulfilled expectation, then passed
  with `deny(warnings)` after removing it. The actual Apple module still needs
  its native build.
- Confirm whether to retain AVFoundation `devicesWithMediaType` for the
  supported macOS 12.3 floor, or migrate discovery without losing cameras on
  older systems. Its existing reason identifies macOS 14+ device-type constants
  as the compatibility constraint. Do not raise the deployment floor implicitly.

### Results (m1, 2026-10-23)

- Conditional Rust suppressions were inspected. The feature-dependent enum
  layouts, pointer-width gates, libc-dependent casts, conditional crypto
  fallbacks, and benchmark modules compiled without test bodies still explain
  their allows. The one reproduced stale Apple naming expectation is removed.
  Dead-code and unused suppressions remain with the dead-code audit.
- TypeScript deprecations: 877 files under `js`, `demo`, `bench`, and `test`
  passed `@typescript-eslint/no-deprecated`. The first 851 used their existing
  projects; 26 files outside projects passed a second sweep with an explicit
  temporary `allowDefaultProject` list. No repository TS configuration changed.
  The disposable toolchain was typescript-eslint 8.72.0, TypeScript 6.0.3,
  ESLint 9.39.5. The [supported TypeScript range](https://typescript-eslint.io/users/dependency-versions/)
  remains `>=4.8.4 <6.1.0`, so a permanent TS 7 deprecation gate is still premature.
- Python: removed 20 unnecessary type ignores, reproduced by pyright against
  freshly generated bindings. The wrapper now rejects unnecessary ignores in
  its existing check. Both wrapper and CARLA passed the audit with generated
  bindings installed. CARLA cannot enable this gate yet: its standalone check
  removes the installed bindings, loses type information, and falsely marks
  its three necessary ignores unused. Retained ignores cover intentional
  invalid calls or UniFFI error-class rebinding.
- Existing stale-suppression gates remain: Rust lint reasons and unconditional
  `expect`, Biome unused suppressions, and Ruff `RUF100`. Go, Kotlin, and Dart
  deprecation gates were clean in m0; this run found no additional hand-written
  suppression/deprecation markers in those wrappers or C/C++ requiring changes.

Count with `git grep` over tracked files, including multi-line Rust attributes.
Generated/vendor bindings retain generator-owned suppressions.
