# [M] The merge gate lints macOS and Windows code from Linux

## Goal

`just ci`, which the merge queue runs on its Linux runner, runs clippy on the
changed Rust crates and their dependents for macOS and Windows targets, so a
platform-gated compile error or unfulfilled lint expectation fails the queue
instead of the nightly macOS and Windows lanes after it lands. Tests stay on
Linux, and real macOS and Windows runners stay out of the gate.

## Plan

From moq-dev/quest's 2026-11-18 mistake audit
([moq-dev/quest#786](https://github.com/moq-dev/quest/pull/786)): 10 runtime
escapes came from Linux-only behavior or platform-gated code that only the
nightly lanes, or no lane, exercised. Six of them were `just check` failures
on macOS, fixed by [moq-dev/moq#8538](https://github.com/moq-dev/moq/pull/8538).
Two of those were unfulfilled `#[expect]`s that only a macOS build saw, one of
them a clippy lint.

Decided 2026-11-19 (maintainer):

- ✅ A cheap cross-target check from Linux in the merge gate (rejected: real
  macOS and Windows runners in the gate; drop). The pick of
  [moq-dev/quest#786](https://github.com/moq-dev/quest/pull/786).
- ✅ clippy, not `cargo check` (rejected: `cargo check`). Unfulfilled clippy
  expectations are part of the class, and clippy costs about what check does.
  Scope is not part of that choice. It stays: changed crates plus dependents.

Recommended:

- Reuse `sh/rs/select.sh`'s `platform` selection and flags (moq-gst excluded,
  the cli's device features on). Today it runs `cargo check` on native hosts
  through `just rs windows`/`macos`. The gate's run lints with
  `--target` instead.
- Targets: `aarch64-apple-darwin`, and `x86_64-pc-windows-msvc`, the target
  `release-binary.yml` ships. The [code audit](/quest/m3/audit-code.md) already
  ran clippy for macOS and `x86_64-pc-windows-gnu` from Linux with a
  rust-overlay toolchain and `zig cc` for build scripts. Use the gnu target
  only if msvc's SDK cannot come from Nix, and say why in the PR.
- A crate whose build script cannot cross-build fails loud. If it must be
  left out, exclude it by name with the reason beside the exclusion, as
  moq-gst is today; the nightly native lanes still cover it.
- Measure what it adds to a queue run, both a typical diff and a moq-net
  edit. Put the measurements in the PR, as the [CI epic](/quest/m0/ci/README.md)
  asks of a change that slows the gate.
- Show it catching a known break, such as reverting one of #8538's
  `#[expect]` fixes.
- Once it gates, delete the code audit's step that runs the cross-target
  clippy by hand. Also update `CONTRIBUTING.md`'s advice to dispatch
  `platform.yml` for cfg-gated code, which then only needs dispatching to
  run tests.

## Related

- [Code audit](/quest/m3/audit-code.md) - runs the same cross-target clippy by hand today
