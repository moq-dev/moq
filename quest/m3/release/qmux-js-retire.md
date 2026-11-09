# [XS] Retire the web-transport JS qmux

## Goal

After the first `@moq/qmux` release built from `js/qmux` is on npm,
`@moq/web-socket-stream` is deprecated on npm with a pointer to
`@moq/qmux`, and moq-dev/web-transport no longer publishes `@moq/qmux`, so
one repository owns the npm name.

Check with `npm view @moq/qmux repository` (names moq-dev/moq) and
`npm view @moq/web-socket-stream deprecated`.

## Plan

Decided 2026-10-20 (maintainer): moq-dev/web-transport stops publishing
`@moq/qmux` once moq's first release of it is on npm. Waiting on that
publication and a maintainer with npm rights:

- `npm deprecate @moq/web-socket-stream "Folded into @moq/qmux; see https://github.com/moq-dev/moq/blob/main/js/qmux/MIGRATE.md"`.
- Remove or freeze `js/qmux` and `js/web-socket-stream` in moq-dev/web-transport,
  whose release script would otherwise publish a competing `@moq/qmux`.

Decided 2026-11-08 (maintainer, /quest-unblock): ✅ freeze at the cut
(rejected: freeze now; skip), alongside
[qmux-rs-retire](/quest/m3/release/qmux-rs-retire.md). The freeze is a PR
an agent opens; only merging it and the npm deprecation need the maintainer.

## Waiting

- moq-dev/moq: the next breaking cut starts. As the cut's first step, an
  agent opens a moq-dev/web-transport PR that drops the `@moq/qmux` and
  `@moq/web-socket-stream` steps from its `release-js.yml`, so it no longer
  publishes either; the maintainer merges it before the cut publishes
  `@moq/qmux` from this repository. Done when web-transport's release config
  no longer publishes `@moq/qmux`.
- Maintainer: with npm rights, run the deprecation above once the first moq
  release of `@moq/qmux` is on npm, so the migration guide never points at
  web-transport's 0.3.4; an agent has no npm rights. Done when
  `npm view @moq/qmux repository` names moq-dev/moq and
  `npm view @moq/web-socket-stream deprecated` is set.
