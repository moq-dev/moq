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
own deprecated exports or hidden CLI spellings is a breaking change, so it is
also a follow-up.

### Gates already in place

Every workspace crate sets `lints.workspace = true` (moq-quic joined in the
m1 run, dropping quinn's extra lints), and the root `[workspace.lints]`
enables `clippy::allow_attributes_without_reason`. Unconditional
suppressions are `expect`, so a stale one fails clippy as
`unfulfilled_lint_expectations`. Biome reports an unused `biome-ignore` as
`suppressions/unused`, ruff selects `RUF100` (root and `demo/carla/sim`),
and pyright's `reportUnnecessaryTypeIgnoreComment` is an error for the
Python wrapper. This run only reviews what those gates can't see.

### Check first

- Lanes clippy never runs. Linux CI lints default and all features only, and
  the macOS and Windows platform lanes run `cargo check`, so an `expect` that
  holds only on Linux, or only with a feature on, passes CI while failing
  elsewhere. Run clippy for `aarch64-apple-darwin` and
  `x86_64-pc-windows-gnu` from Linux: a scratch rust-overlay toolchain
  carrying those std targets, `zig cc` as the C and C++ compiler for crates
  with build scripts (`-target aarch64-macos` after dropping `-arch` and
  `-mmacosx-version-min`), and the platform lane's features plus
  `moq-video/render`. That found every stale suppression the m1 run fixed.
- Windows clippy still reports dead code the [dead code
  audit](/quest/m2/audit-dead-code.md) owns: `capture::pump::Read::Idle`
  (only X11 holds a capture idle) and `I420::from_bgra`, whose cfg and doc
  still name Windows Desktop Duplication though only PipeWire calls it.
- Swift: the nightly macOS platform job builds the wrapper and tests with
  Xcode. Read its Swift step for compiler warnings rather than building on
  Linux, which has no Apple SDK.
- TypeScript deprecations: typescript-eslint supported TypeScript
  `>=4.8.4 <6.1.0` at the m1 run, so `no-deprecated` ran from a throwaway
  package pinning typescript 6 and eslint 9, with an explicit
  `allowDefaultProject` list for files outside any `tsconfig`. Check whether
  it supports TS 7 before repeating that; if it does, consider joining CI.
- The CARLA sim's pyright check removes the installed bindings, so it can't
  enable `reportUnnecessaryTypeIgnoreComment` without falsely flagging its
  three needed ignores. Audit it with the bindings installed.

### Results (m1, 2026-10-24)

- Rust: removed the Apple-only `msghdr_x` `non_camel_case_types` (Rust
  exempts `repr(C)` structs) and a VideoToolbox `too_many_arguments` on a
  seven-argument function. Clock expectations that held only on Linux or with
  a feature moved onto the items that read the clock: moq-sock's shard and
  UDP tests, moq-uring's benches, moq-video's PipeWire render test, and the
  relay's Unix signal test. Windows-only code that read the host clock
  unsuppressed (a `frame` GPU workload test and the `d3d11-resize-probe`
  example) got `expect`s, moq-cli's `notify_ready` lost a `needless_return`
  off Unix, and two Linux-only ACME test helpers got their callers' cfg. A
  moq-mux bench `allow(dead_code)` lacked its reason, which also failed
  Linux clippy.
- Deprecations: the AVFoundation `devicesWithMediaType` stays. Its
  replacement needs `AVCaptureDeviceTypeExternal` (macOS 14+) for external
  cameras, and the macOS 12.3 floor's alternative,
  `AVCaptureDeviceTypeExternalUnknown`, is itself deprecated. The Swift
  wrapper built without warnings under Xcode 26.6 in the 2026-10-22 nightly;
  its later changes were FFI-only. 877 TypeScript files passed
  `@typescript-eslint/no-deprecated`.
- Python: removed 22 unnecessary type ignores and made the wrapper's pyright
  reject new ones.

### Baselines (2026-10-24, after the m1 run)

- Rust `clippy::` suppression attributes: 191 (167 `expect`, 14 `cfg_attr`
  `expect`, 10 `allow`). Most common: `disallowed_methods` 128,
  `disallowed_types` 50, `large_enum_variant` 23.
- Rust rustc suppressions: 51 (31 `expect`, 12 `cfg_attr` `expect`, 8
  `allow`), led by `non_camel_case_types` 17 and `irrefutable_let_patterns`
  12. `dead_code` and `unused`: 412, owned by the dead code audit.
- Rust `deprecated`: 1, the AVFoundation call above.
- TypeScript: 29 `biome-ignore` and 47 `@ts-expect-error`.
- Python: 8 `noqa` or `type: ignore`.

Count with `git grep` over the whole tree, not per language directory, so
only tracked files count and none outside `rs/`, `js/`, or `py/` are missed.
Count multi-line Rust attributes too, since rustfmt wraps long ones.
Generated UniFFI bindings are untracked and carry their own suppressions;
leave them out, and fix a noisy one in the generator config instead of the
output.

## Required

- [Release cut](/quest/m2/release/cut.md) - the release in flight ships before this run starts
