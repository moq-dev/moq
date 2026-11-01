# [S] js/qmux publishes as npm qmux

## Goal

`js/qmux` is the npm package `qmux` instead of `@moq/qmux`, so the next cut
publishes the JS QMux library under the same name as the Rust `qmux` crate.
Every workspace dependent, test, and doc uses the new name, and nothing in the
repository still publishes or imports `@moq/qmux`.

## Plan

Decided 2026-11-01 (maintainer, via `/quest-plan`, a follow-up of
[moq-dev/moq#7582](https://github.com/moq-dev/moq/pull/7582)):

- Name: unscoped npm `qmux`, without JSR, because QMux is not MoQ-specific.
  JSR has no unscoped names, so the package sets `"jsr": false` and the JSR
  copy of `@moq/qmux` stops getting releases.
  - npm `qmux`, no JSR ✅
  - `@qmux/qmux` on npm + JSR
  - npm `qmux` + JSR `@qmux/qmux`
- Version: the npm package's minor follows the Rust `qmux` crate's (0.7.0 on
  main, unpublished), so its first release is 0.7.0, and one "qmux 0.7" names
  both implementations. Patches stay independent. The
  [shared-version check](/quest/m1/shared-version.md) enforces the match.
  - Match the Rust qmux crate ✅
  - Continue @moq/qmux's line (0.4.0)
- Timing: m0, so the rename lands before moq ever publishes `@moq/qmux` from
  `js/qmux`; the old name never gets a release from this repository.
  - m1, before the cut
  - m2, after the cut
  - m0, "before somebody takes qmux" ✅

Rename `name` in `js/qmux/package.json`, set `"jsr": false` and the version,
and update every reference: the `@moq/net` and `@moq/p2p` dependencies and
imports, `test/qmux` (rename its package to `qmux-interop`), `test/max-age`,
`doc/.vitepress/api/package.json`, `bun.lock`, the API report under
`js/qmux/api`, `js/qmux/README.md`, `test/README.md`, and the JavaScript list
in `doc/setup/upgrade.md`. Add a `MIGRATE.md` entry: install `qmux`, import
from `qmux`, with no API change. Leave historical `CHANGELOG.md` lines alone.

The publish needs the [name claim](/quest/m0/qmux-npm-claim.md) before the
cut, but this code change does not wait for it.

Public API: package name only. Wire: none.

## Related

- [Retire the web-transport JS qmux](/quest/m2/release/qmux-js-retire.md) - deprecates `@moq/qmux` and `@moq/web-socket-stream` once `qmux` publishes
