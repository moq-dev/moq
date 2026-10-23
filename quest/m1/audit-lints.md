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

### What the m0 run left in place

Every workspace crate but `moq-quic` sets `lints.workspace = true`, and the
root `[workspace.lints]` enables `clippy::allow_attributes_without_reason`, so a
suppression without a reason fails clippy. Unconditional suppressions are
`#[expect(..., reason = "...")]`, so a stale one fails as
`unfulfilled_lint_expectations`. A crate needing extra lints gets its own
manifest `[lints]` tables that repeat that rule (`moq-quic/Cargo.toml`), since
Cargo can't extend the workspace table per crate and crate attributes skip
benches and examples. Biome reports an unused `biome-ignore` as
`suppressions/unused`, and ruff selects `RUF100` (root and
`demo/carla/sim`). This run only reviews what those gates can't see.

### Check first

- Conditional suppressions: every `cfg_attr(..., allow(...))` and every
  plain `allow` with a reason. Ask whether the condition still holds or the
  lint now fires everywhere (then `expect`) or nowhere (then delete). The
  wasm lane also lints `moq-net` and `moq-flate` as dependencies, so a
  suppression there must hold on wasm32 too.
- Lanes clippy never runs: the macOS and Windows platform lane is
  `cargo check` only, so an `expect` on Apple or Windows code
  (`moq-video` AVFoundation, `moq-sock`) is unverified. Run clippy there or
  confirm by reading.
- Python type ignores: run pyright with
  `reportUnnecessaryTypeIgnoreComment`, which m0 didn't.
- Swift: grep and build for deprecation warnings, which m0 didn't.
- TypeScript deprecations: typescript-eslint doesn't support the repo's TS 7
  yet, so m0 swept `no-deprecated` from a throwaway package pinning
  typescript 6 and eslint 9. Files outside any `tsconfig` project can't be
  typed-linted. Check whether typescript-eslint supports TS 7 before
  repeating that setup; if it does, consider joining CI.

### Results (m0, 2026-10-20)

- Rust: workspace lints adopted, every suppression given a reason, and
  stale ones removed: 27 `match_like_matches_macro` (now `matches!`), 27
  `non_camel_case_types` on `repr(C)` structs, `moq-quic`'s crate-wide
  `cognitive_complexity` and `too_many_arguments`, and stale
  `type_complexity`, `arc_with_non_send_sync`, `field_reassign_with_default`,
  `disallowed_methods`, and pedantic allows.
- Deprecations: `js/publish` file import moved to mediabunny's async track
  getters. The `moq-relay` cluster test's `#[allow(deprecated)]` was stale.
  Go (staticcheck `SA1019`), Kotlin, and Dart (`--fatal-infos`) were clean.

### Baselines (2026-10-20, after the m0 run)

- Rust `clippy::` suppression attributes: 189 (168 `expect`, 12 `cfg_attr`
  `expect`, 9 `allow`), counted 2026-10-22 after main's moq-clock move.
  Most common: `disallowed_methods` 123, `disallowed_types` 48,
  `large_enum_variant` 24, mostly test modules reading the host clock.
- Rust rustc and rustdoc suppressions: 64 (49 `expect`, 15 `allow`), led by
  `non_camel_case_types` 19 on C ABI enums. `dead_code` and `unused`: 421,
  owned by the dead code audit.
- Rust `deprecated`: 1, the macOS AVFoundation `devicesWithMediaType`.
- TypeScript: 40 `biome-ignore` and 47 `@ts-expect-error`.
- Python: 26 `noqa` or `type: ignore`.

Count with `git grep` over the whole tree, not per language directory, so
only tracked files count and none outside `rs/`, `js/`, or `py/` are missed.
Count multi-line Rust attributes too, since rustfmt wraps long ones.
Generated UniFFI bindings are untracked (`kt/.gitignore`) and carry their own
suppressions, such as Kotlin's 139 `@Suppress`; leave them out, and fix a
noisy one in the generator config instead of the output.
