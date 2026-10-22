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
emit clean code.

A suppression stays only when fixing the lint would make the code worse; a
signature reshaped to satisfy `too_many_arguments` or `large_enum_variant`
on a public type is an API change, so it is a follow-up. Removing one of our
own deprecated exports or hidden CLI spellings is a breaking change with a
`MIGRATE.md` entry, so it is also a follow-up.

### Enforcement (first run only)

Make a stale Rust suppression fail loudly instead of waiting for the next run
(decided 2026-10-20). Convert each unconditional `#[allow(clippy::...)]` to
`#[expect(clippy::..., reason = "...")]`, and add a `[workspace.lints]`
table enabling `clippy::allow_attributes_without_reason` that every crate
opts into with `lints.workspace = true`. `moq-net`, `moq-quic`, and `kio`
already have their own `[lints]` tables, which must merge into the
workspace one. A suppression needed under only some features or platforms
uses `cfg_attr` or stays an `allow` with its reason, since an `expect` fails
on the builds where the lint never fires. Later runs then only review
reasons and the conditional allows.

Other languages are audited by hand each run. TypeScript already uses
`@ts-expect-error` everywhere (no `@ts-ignore`), which errors when unneeded;
check whether Biome reports unused `biome-ignore` comments, and whether
ruff's `RUF100` (unused `noqa`) is worth adding to the root `pyproject.toml`
selection.

### Deprecations

Rust deprecation warnings already fail under `-D warnings`, so look where
that gate is blind: `#[allow(deprecated)]`, and targets CI doesn't build
(the macOS AVFoundation capture path). `tsc` reports deprecated use only as
an editor hint, so sweep TypeScript once with a typed linter such as
typescript-eslint's `no-deprecated`, outside the package dependency graph
unless it can join CI cheaply. Search each binding's toolchain for its
deprecation output too.

### Baselines (2026-10-20)

- Rust: 147 `clippy::` allow/expect attributes (9 already `expect`). Most
  common: `disallowed_methods` 37 and `disallowed_types` 13 (the ambient
  clock guard in 10 `clippy.toml` files; benchmarks and the `moq-clock`
  runtime opt out), `match_like_matches_macro` 23, `large_enum_variant` 19,
  `too_many_arguments` 10, `result_large_err` 10, `unreadable_literal` 9,
  `type_complexity` 8, `arc_with_non_send_sync` 7. `moq-quic` allows
  `cognitive_complexity` and `too_many_arguments` crate-wide.
- Rust `#[allow(deprecated)]`: 2, in a `moq-relay` cluster test and
  `moq-video` AVFoundation capture. No `#[deprecated]` items of our own.
- TypeScript: 39 `biome-ignore` and 47 `@ts-expect-error`, of which 6 and 2
  sit outside `js/` (`demo/carla`, `sh/gh`, `test/audio-quality`,
  `test/smoke`). No `@deprecated` exports.
- Python: 30 `noqa` or `type: ignore`, 24 of them in `py/moq-rs/tests`.
  Kotlin, Go, Swift, Dart, and C/C++ had none found by a plain grep; confirm
  with each toolchain's own syntax.

Count with `git grep` over the whole tree, not per language directory, so
only tracked files count and none outside `rs/`, `js/`, or `py/` are missed. Generated UniFFI bindings
are untracked (`kt/.gitignore`) and carry their own suppressions, such as
Kotlin's 139 `@Suppress`; leave them out, and fix a noisy one in the generator
config instead of the output.
