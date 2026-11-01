# [M] Every package shares one minor version

## Goal

Every published MoQ package, in every ecosystem (Rust crates, npm `@moq/*`,
Python, Swift, Kotlin, Dart, Go, C++, and OBS), ships with the same minor
version, so a release is announced as one number. The next breaking cut
publishes everything at `0.22.0`, and each later cut bumps them together to
the next minor, including packages that did not change.

This quest delivers the tooling, check, and docs; the
[cut](/quest/m2/release/cut.md) applies the version. Patch versions stay
independent per package between cuts. Experiments stay on `0.0.x`.

## Plan

Decided 2026-11-01 (maintainer, via `/quest-plan`), for marketing: one minor
across the whole project.

- Scope: every published package listed in CONTRIBUTING.md's Versions
  section, not just Rust crates.
  - Rust crates only
  - Every package ✅ ("we can build some automation if needed")
- Shared version: `0.22.0`, the lowest above every published package
  (crates.io has hang 0.21.19 and moq-native 0.20.0, so 0.20 is taken; every
  npm, PyPI, pub.dev, and OBS release is below 0.7).
  - 0.22.0 ✅
  - 0.30.0
- Patch versions are independent, for Rust too: release-plz keeps bumping
  each crate's patch on its own, so moq-net 0.22.3 beside hang 0.22.1 is
  fine. No `version_group`; it lines up the patch versions too, which is not
  wanted. ([moq-dev/moq#629](https://github.com/moq-dev/moq/pull/629) added
  groups and [moq-dev/moq#726](https://github.com/moq-dev/moq/pull/726)
  removed them without a recorded reason.)
  - version_group
  - Independent patches ✅ ("patch versions are not lockstep")
- Exempt by name, keeping their own versions: `kio`, `kio-macros`, `qmux`,
  and npm `@moq/qmux`, because they are not MoQ-specific. `@moq/signals`
  joins. Renaming `@moq/qmux` to a non-`@moq` name is a separate follow-up.
  - Join the group
  - Exempt by name ✅ (Rust); npm: exempt `@moq/qmux` only ✅
- New packages start at `0.0.x`. A `0.0.x` package that a published `0.x.y`
  library depends on (normal or build dependency, same ecosystem) must join
  the shared version. Only library dependents count, because a binary-only
  package exposes no API downstream (decided 2026-11-01). Today that promotes
  `moq-uring` (the `moq_relay` library's optional dependency). `moq-archive`
  stays `0.0.x`: its only dependents are the binary-only moq-cli and a moq-hls
  dev-dependency. Every other `0.1+` crate already has a `0.x.y` dependent.
  Unpublished packages (`publish = false`, npm `private`) are exempt from
  both rules.
  - Only library deps count ✅
  - Any dependent counts
- Enforcement: a CI check in `just check`.
  - CI check ✅
  - Docs only
- Bump timing: at the cut, not in this quest's PR, so the maintainer's
  pre-cut merge of `release` into `main` doesn't conflict on every version
  line and lockfile.
  - In this quest's PR
  - At the cut ✅
- Milestone: m1, since it must land before the cut.

Implementation guidance:

- One recipe, for example `just version 0.22.0`, sets every eligible
  package: `release-plz set-version` for each crate (which also rewrites
  dependents' requirements), each `js/*/package.json` with a `release` script
  plus `bun.lock`, `py/moq-rs/pyproject.toml` plus `uv.lock`, `swift/VERSION`,
  `moq.version` in `kt/gradle.properties`, `dart/moq/pubspec.yaml`,
  `go/wrapper/VERSION` (`0.22`), `cpp/moq/VERSION`, and `cpp/obs/VERSION`.
  Packages that track the moq-ffi crate (py/moq-ffi, the FFI artifacts) need
  no edit. The recipe also records the shared minor in one place, such as a
  root file, for the check to read.
- The check reads Cargo metadata and the JS workspace, along with the other
  version files. It fails when an eligible package's minor differs from the
  recorded one, when a `0.0.x` package has a published `0.x.y` library dependent, or
  when an exemption names a package that no longer exists. Until the cut
  records the shared minor, the check passes without enforcing (the bump
  waits for the cut). Add a regression test for each failure.
- Verify that `release-plz release-pr` publishes a hand-set, unpublished
  `0.22.0` as is, without bumping it again from breaking commits. If it
  doesn't, the recipe must run where release-plz respects it, for example
  inside the release PR.
- Update CONTRIBUTING.md's Versions section with the shared-minor rule, the
  exemptions, the `0.0.x` rule, and the recipe. Update its release bullets so
  a cut runs the recipe.

Public API: none. Wire: none.

## Related

- [Cut and publish](/quest/m2/release/cut.md) - runs the recipe right after the pre-cut merge of `release` into `main`
- [Retire the web-transport JS qmux](/quest/m2/release/qmux-js-retire.md) - owns the `@moq/qmux` npm name this exempts
